// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ping` — the SIG's counterpart to Fedora's `needinfo?`, for a
//! change that three different people own: the SIG member who builds
//! the Proposed Update, the author of the merge request that carries
//! it upstream, and the CentOS Stream maintainer who has to review
//! that merge request. Each open tracking issue is read against its
//! upstream MR, the SIG's builds and stock Stream, and each party gets
//! the message that is theirs, where they read, in the order that
//! makes the next one actionable:
//!
//! - **rebase-build** — stock Stream has moved past the SIG's build:
//!   the SIG rebuilds first. A note on the tracking issue says so,
//!   once per stock build.
//! - **rebase-mr** — the MR no longer merges cleanly: a note on the MR
//!   tells its author it is behind its target, once per head revision.
//! - **announce** — a SIG build has reached `-testing` or `-release`
//!   and no note has named it at that stage: a "for those watching"
//!   note gives the NVR and how to get it, on the MR and on the
//!   tracking issue, once per build and stage.
//! - **ping** — the MR merges cleanly, the SIG build is current, and
//!   nobody has touched the MR for the window: a note asks the
//!   maintainer what blocks review, and again after `--reping-days`
//!   if it stays unanswered.
//!
//! When someone upstream spoke last the SIG owes the reply, so the
//! last response is shown instead. Read-only by default; `--apply`
//! posts. Every note carries a hidden marker so later runs recognise
//! it.

use std::process::ExitCode;

use chrono::{DateTime, Utc};

use crate::gitlab::{self, Note};
use crate::status::{
    fetch_proposed_updates_nvrs, fetch_proposed_updates_testing_nvrs, fetch_stream_nvrs,
    parse_mr_line, scan_mr_url_in_body, stream_newer_than_proposed, tracking_project_of,
};
use crate::utils::gitlab_base;

const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";
const TRACKING_LABEL: &str = "cpu-sig-tracker";
/// Hidden in the rendered notes; how a later run recognises them.
pub const PING_MARKER: &str = "<!-- cpu-sig-tracker: ping -->";
const REBASE_MARKER: &str = "<!-- cpu-sig-tracker: rebase ";
const BUILD_MARKER: &str = "<!-- cpu-sig-tracker: build ";
const BEHIND_MARKER: &str = "<!-- cpu-sig-tracker: behind ";

#[derive(clap::Args)]
pub struct PingArgs {
    /// Path to the sandogasa-inventory TOML file (its workloads name
    /// the releases to scan).
    #[arg(short, long, default_value = "inventory.toml")]
    pub inventory: String,

    /// Restrict the scan to a single release (e.g. `c10s`).
    #[arg(long)]
    pub release: Option<String>,

    /// Restrict to these package(s) (repeat/CSV).
    #[arg(long = "package", value_name = "PKG", value_delimiter = ',')]
    pub packages: Vec<String>,

    /// Days without upstream activity before a merge request is
    /// pinged (default 14).
    #[arg(
        long,
        value_name = "N",
        default_value = "14",
        hide_default_value = true
    )]
    pub days: i64,

    /// Days after an unanswered ping before it is repeated (default
    /// 30).
    #[arg(
        long,
        value_name = "N",
        default_value = "30",
        hide_default_value = true
    )]
    pub reping_days: i64,

    /// Post the notes; without it, report what would be posted.
    #[arg(long)]
    pub apply: bool,

    /// Emit a machine-readable JSON array instead of a table.
    #[arg(long)]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

/// What the merge request needs from whom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Stock Stream is past the SIG's build: the SIG rebuilds first.
    RebaseBuild,
    /// The MR has conflicts: its author rebases.
    RebaseMr,
    /// Someone upstream spoke last: the SIG owes the reply.
    Respond,
    /// Quiet for long enough with no ping outstanding, or the last
    /// ping is older than the re-ping window: ping the maintainer.
    Ping,
    /// Our ping stands unanswered, within the re-ping window.
    Waiting,
    /// Touched recently; nothing to do yet.
    Active,
    /// Merged or closed upstream.
    Closed,
}

/// The last thing a person other than us wrote on the MR.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Response {
    pub by: String,
    pub at: DateTime<Utc>,
    /// The note's first line.
    pub excerpt: String,
}

/// A build to tell the watchers about, and where it has not been said.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Announce {
    pub nvr: String,
    /// `testing` or `release` — the CBS tag it reached.
    pub stage: &'static str,
    pub to_mr: bool,
    pub to_issue: bool,
}

/// What we know about one tracked change.
#[derive(Debug, Clone, Default)]
pub struct Facts<'a> {
    pub mr_state: &'a str,
    pub has_conflicts: bool,
    /// Head sha of the MR, for the once-per-revision rebase note.
    pub sha: &'a str,
    pub updated_at: Option<DateTime<Utc>>,
    pub mr_notes: &'a [Note],
    /// Notes on the SIG's tracking issue.
    pub issue_notes: &'a [Note],
    /// The SIG's build tagged into `-release`, if any.
    pub release_nvr: Option<&'a str>,
    /// The SIG's build tagged into `-testing`, if any.
    pub testing_nvr: Option<&'a str>,
    /// Stock Stream's build.
    pub stream_nvr: Option<&'a str>,
}

/// The windows the decision uses, in days.
#[derive(Debug, Clone, Copy)]
pub struct Windows {
    pub quiet: i64,
    pub reping: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Assessment {
    pub action: Action,
    /// Whether the tracking issue still has to be told the build is
    /// behind stock (`rebase-build` only).
    pub tell_issue_behind: bool,
    /// Builds to announce, and where.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub announce: Vec<Announce>,
    /// When the MR last saw any activity, ours included.
    pub last_activity: DateTime<Utc>,
    pub quiet_days: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinged_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_response: Option<Response>,
}

#[derive(Debug, serde::Serialize)]
pub struct Row {
    pub release: String,
    pub package: String,
    pub issue_url: String,
    pub mr_url: String,
    pub mr_state: String,
    pub mr_author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_nvr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub testing_nvr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_nvr: Option<String>,
    #[serde(flatten)]
    pub assessment: Assessment,
    /// Notes `--apply` posted on this run, as `what@where`.
    pub posted: Vec<String>,
}

fn author(n: &Note) -> &str {
    n.author.as_ref().map_or("", |a| a.username.as_str())
}

fn parse_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// When we last left a note carrying `marker`, if ever.
fn ours(notes: &[Note], me: &str, marker: &str) -> Option<DateTime<Utc>> {
    notes
        .iter()
        .filter(|n| !n.system && author(n) == me && n.body.contains(marker))
        .filter_map(|n| parse_time(n.created_at.as_deref()))
        .max()
}

fn build_marker(nvr: &str, stage: &str) -> String {
    format!("{BUILD_MARKER}{nvr} {stage} -->")
}

/// Decide what the change needs. `me` is the token's login, so our
/// own notes are told from upstream's; the MR's `updated_at` moves on
/// pushes and label changes as well as notes, so it counts as
/// activity too.
pub fn assess(f: &Facts<'_>, me: &str, now: DateTime<Utc>, w: Windows) -> Assessment {
    let human: Vec<&Note> = f.mr_notes.iter().filter(|n| !n.system).collect();
    let last_note_at = human
        .iter()
        .filter_map(|n| parse_time(n.created_at.as_deref()))
        .max();
    let last_activity = [f.updated_at, last_note_at]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(now);
    let quiet_days = (now - last_activity).num_days();
    let pinged_at = ours(f.mr_notes, me, PING_MARKER);
    let last_other = human.iter().rev().find(|n| author(n) != me);
    let last_response = last_other.map(|n| Response {
        by: author(n).to_string(),
        at: parse_time(n.created_at.as_deref()).unwrap_or(now),
        excerpt: n.body.lines().next().unwrap_or("").trim().to_string(),
    });
    let ours_last = human.last().is_some_and(|n| author(n) == me);
    let build_behind = stream_newer_than_proposed(f.release_nvr, f.stream_nvr);
    let rebase_said = ours(f.mr_notes, me, &format!("{REBASE_MARKER}{} -->", f.sha)).is_some();
    let ping_due = match pinged_at {
        None => true,
        Some(t) => (now - t).num_days() >= w.reping,
    };
    let action = if f.mr_state != "opened" {
        Action::Closed
    } else if build_behind {
        Action::RebaseBuild
    } else if f.has_conflicts && !rebase_said {
        Action::RebaseMr
    } else if last_other.is_some() && !ours_last {
        Action::Respond
    } else if quiet_days < w.quiet || f.has_conflicts {
        Action::Active
    } else if ping_due {
        Action::Ping
    } else {
        Action::Waiting
    };
    let tell_issue_behind = action == Action::RebaseBuild
        && ours(
            f.issue_notes,
            me,
            &format!("{BEHIND_MARKER}{} -->", f.stream_nvr.unwrap_or("")),
        )
        .is_none();
    // A build is announced at each stage it reaches, once; the same
    // NVR in both tags is announced as released only.
    let mut announce = Vec::new();
    if f.mr_state == "opened" && !build_behind {
        let stages = [(f.release_nvr, "release"), (f.testing_nvr, "testing")];
        for (nvr, stage) in stages {
            let Some(nvr) = nvr else { continue };
            if stage == "testing" && f.release_nvr == Some(nvr) {
                continue;
            }
            let marker = build_marker(nvr, stage);
            let to_mr = ours(f.mr_notes, me, &marker).is_none();
            let to_issue = ours(f.issue_notes, me, &marker).is_none();
            if to_mr || to_issue {
                announce.push(Announce {
                    nvr: nvr.to_string(),
                    stage,
                    to_mr,
                    to_issue,
                });
            }
        }
    }
    Assessment {
        action,
        tell_issue_behind,
        announce,
        last_activity,
        quiet_days,
        pinged_at,
        last_response,
    }
}

/// The note asking the maintainer what blocks review.
pub fn ping_body(last_activity: DateTime<Utc>, quiet_days: i64, issue_url: &str) -> String {
    format!(
        "Friendly ping from the CentOS Proposed Updates SIG: this merge request has \
         had no activity since {} ({quiet_days} days). Is anything blocking review, or \
         is there something the SIG can do to help it land?\n\n\
         Until it does, the SIG carries this change as a Proposed Update, tracked at \
         {issue_url}.\n\n{PING_MARKER}\n",
        last_activity.format("%Y-%m-%d")
    )
}

/// The note telling the MR's author it is behind its target.
pub fn rebase_body(mr_author: &str, target: &str, sha: &str) -> String {
    format!(
        "@{mr_author}: heads-up from the CentOS Proposed Updates SIG — `{target}` has \
         moved on and this merge request no longer merges cleanly (GitLab reports \
         conflicts). A rebase onto current `{target}` would let review proceed; the \
         SIG is rebuilding its Proposed Update alongside.\n\n{REBASE_MARKER}{sha} -->\n"
    )
}

/// The note on the tracking issue when stock has passed the SIG's build.
pub fn behind_body(stream_nvr: &str, release_nvr: &str, release: &str) -> String {
    format!(
        "Stock CentOS Stream {} now carries `{stream_nvr}`, ahead of the SIG's \
         `{release_nvr}`: this Proposed Update needs a rebase and rebuild before \
         anything else moves.\n\n{BEHIND_MARKER}{stream_nvr} -->\n",
        stream_of(release)
    )
}

/// The note announcing a SIG build to whoever follows the change.
pub fn announce_body(nvr: &str, stage: &str, package: &str, release: &str) -> String {
    let stream = stream_of(release);
    let how = if stage == "release" {
        format!(
            "To install it:\n\n```\ndnf install centos-release-proposed_updates\n\
             dnf update {package}\n```"
        )
    } else {
        format!(
            "It is in the SIG's testing repository, \
             https://buildlogs.centos.org/centos/{stream}-stream/proposed_updates/$basearch/packages-main/, \
             for anyone who can try it before it is released."
        )
    };
    let verb = if stage == "release" {
        "released"
    } else {
        "built"
    };
    format!(
        "For those watching: the CentOS Proposed Updates SIG has {verb} `{nvr}` for \
         CentOS Stream {stream}, carrying this change. {how}\n\n{}\n",
        build_marker(nvr, stage)
    )
}

/// `c10s` → `10`.
fn stream_of(release: &str) -> &str {
    release.trim_start_matches('c').trim_end_matches('s')
}

fn scan(args: &PingArgs) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
    let inventory = sandogasa_inventory::load(&args.inventory)?;
    let releases: Vec<String> = match &args.release {
        Some(r) if !inventory.inventory.workloads.contains_key(r) => {
            return Err(format!(
                "release '{r}' not found in inventory; available: {:?}",
                inventory.workload_names()
            )
            .into());
        }
        Some(r) => vec![r.clone()],
        None => inventory.inventory.workloads.keys().cloned().collect(),
    };
    let base = gitlab_base();
    let token = gitlab::load_token()?;
    let me = sandogasa_gitlab::current_user(&base, &token)?.username;
    let group = gitlab::group_client(&base, PROPOSED_UPDATES_GROUP)?;
    let now = Utc::now();
    let windows = Windows {
        quiet: args.days,
        reping: args.reping_days,
    };
    let mut rows = Vec::new();
    for release in &releases {
        if args.verbose {
            eprintln!("[ping] fetching open tracking issues for {release}");
        }
        let label = format!("{TRACKING_LABEL},{release}");
        let issues: Vec<(String, gitlab::Issue)> = group
            .list_issues(&label, Some("opened"))?
            .into_iter()
            .filter_map(|i| {
                let package = gitlab::package_from_issue_url(&i.web_url)?.to_string();
                Some((package, i))
            })
            .filter(|(p, _)| args.packages.is_empty() || args.packages.contains(p))
            .collect();
        let packages: Vec<String> = issues.iter().map(|(p, _)| p.clone()).collect();
        let release_nvrs = fetch_proposed_updates_nvrs(release, args.verbose);
        let testing_nvrs = fetch_proposed_updates_testing_nvrs(release, args.verbose);
        let stream_nvrs = fetch_stream_nvrs(release, &packages, args.verbose);
        for (package, issue) in issues {
            let body = issue.description.clone().unwrap_or_default();
            let Some(mr_url) = parse_mr_line(&body)
                .map(|(u, _)| u)
                .or_else(|| scan_mr_url_in_body(&body))
            else {
                eprintln!("warning: {package} {release}: tracking issue names no MR; skipped");
                continue;
            };
            let (_, project, iid) = gitlab::parse_mr_url(&mr_url)?;
            let Some(tracking_project) = tracking_project_of(&issue.web_url) else {
                eprintln!("warning: {package} {release}: unrecognised issue URL; skipped");
                continue;
            };
            if args.verbose {
                eprintln!("[ping] {package} {release}: reading {project}!{iid}");
            }
            let mr_client = gitlab::client(&base, &project)?;
            let issue_client = gitlab::client(&base, &tracking_project)?;
            let mr = mr_client.merge_request(iid)?;
            let mr_notes = mr_client.merge_request_notes(iid)?;
            let issue_notes = issue_client.issue_notes(issue.iid)?;
            let mr_author = mr
                .author
                .as_ref()
                .map(|a| a.username.clone())
                .unwrap_or_default();
            let facts = Facts {
                mr_state: &mr.state,
                has_conflicts: mr.has_conflicts.unwrap_or(false),
                sha: mr.sha.as_deref().unwrap_or(""),
                updated_at: parse_time(mr.updated_at.as_deref()),
                mr_notes: &mr_notes,
                issue_notes: &issue_notes,
                release_nvr: release_nvrs.get(&package).map(String::as_str),
                testing_nvr: testing_nvrs.get(&package).map(String::as_str),
                stream_nvr: stream_nvrs.get(&package).map(String::as_str),
            };
            let assessment = assess(&facts, &me, now, windows);
            let mut posted = Vec::new();
            if args.apply {
                for a in &assessment.announce {
                    let text = announce_body(&a.nvr, a.stage, &package, release);
                    if a.to_mr {
                        mr_client.add_merge_request_note(iid, &text)?;
                        posted.push(format!("announce {}@mr", a.stage));
                    }
                    if a.to_issue {
                        issue_client.add_note(issue.iid, &text)?;
                        posted.push(format!("announce {}@issue", a.stage));
                    }
                }
                match assessment.action {
                    Action::RebaseBuild if assessment.tell_issue_behind => {
                        issue_client.add_note(
                            issue.iid,
                            &behind_body(
                                facts.stream_nvr.unwrap_or(""),
                                facts.release_nvr.unwrap_or(""),
                                release,
                            ),
                        )?;
                        posted.push("behind@issue".to_string());
                    }
                    Action::RebaseMr => {
                        mr_client.add_merge_request_note(
                            iid,
                            &rebase_body(&mr_author, &mr.target_branch, facts.sha),
                        )?;
                        posted.push("rebase@mr".to_string());
                    }
                    Action::Ping => {
                        mr_client.add_merge_request_note(
                            iid,
                            &ping_body(
                                assessment.last_activity,
                                assessment.quiet_days,
                                &issue.web_url,
                            ),
                        )?;
                        posted.push("ping@mr".to_string());
                    }
                    _ => {}
                }
            }
            rows.push(Row {
                release: release.clone(),
                package: package.clone(),
                issue_url: issue.web_url.clone(),
                mr_url,
                mr_state: mr.state.clone(),
                mr_author: mr_author.clone(),
                release_nvr: facts.release_nvr.map(str::to_string),
                testing_nvr: facts.testing_nvr.map(str::to_string),
                stream_nvr: facts.stream_nvr.map(str::to_string),
                assessment,
                posted,
            });
        }
    }
    rows.sort_by(|a, b| (&a.release, &a.package).cmp(&(&b.release, &b.package)));
    Ok(rows)
}

/// One block per change: the action and whose it is, the builds, and
/// the last upstream word when there is one.
pub fn render(rows: &[Row], apply: bool) -> String {
    let verb = |what: &str, done: bool| {
        if done {
            format!("posted {what}")
        } else if apply {
            format!("{what} not posted")
        } else {
            format!("would post {what}")
        }
    };
    let mut out = Vec::new();
    for r in rows {
        let a = &r.assessment;
        let has = |p: &str| r.posted.iter().any(|x| x == p);
        let what = match a.action {
            Action::RebaseBuild => format!(
                "rebase-build — SIG: stock {} is past {}{}",
                r.stream_nvr.as_deref().unwrap_or("?"),
                r.release_nvr.as_deref().unwrap_or("?"),
                if a.tell_issue_behind {
                    format!(
                        "; {}",
                        verb("note on the tracking issue", has("behind@issue"))
                    )
                } else {
                    String::new()
                }
            ),
            Action::RebaseMr => format!(
                "rebase-mr — @{}: conflicts; {}",
                r.mr_author,
                verb("rebase note", has("rebase@mr"))
            ),
            Action::Respond => "respond — SIG owes the reply".to_string(),
            Action::Ping => format!(
                "ping — maintainer: {}{}",
                verb("ping", has("ping@mr")),
                a.pinged_at
                    .map(|t| format!(" (last pinged {})", t.format("%Y-%m-%d")))
                    .unwrap_or_default()
            ),
            Action::Waiting => format!(
                "waiting — pinged {}, no response",
                a.pinged_at
                    .map(|t| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_default()
            ),
            Action::Active => "active".to_string(),
            Action::Closed => r.mr_state.clone(),
        };
        out.push(format!(
            "{} {}: {}\n    {what}; quiet {} days (since {})",
            r.package,
            r.release,
            r.mr_url,
            a.quiet_days,
            a.last_activity.format("%Y-%m-%d")
        ));
        for an in &a.announce {
            let mut where_ = Vec::new();
            if an.to_mr {
                where_.push(verb("on the MR", has(&format!("announce {}@mr", an.stage))));
            }
            if an.to_issue {
                where_.push(verb(
                    "on the tracking issue",
                    has(&format!("announce {}@issue", an.stage)),
                ));
            }
            out.push(format!(
                "    announce {} ({}): {}",
                an.nvr,
                an.stage,
                where_.join(", ")
            ));
        }
        if let Some(resp) = &a.last_response {
            out.push(format!(
                "    last response {} by {}: {}",
                resp.at.format("%Y-%m-%d"),
                resp.by,
                resp.excerpt
            ));
        }
    }
    out.join("\n")
}

pub fn run(args: &PingArgs) -> ExitCode {
    let rows = match scan(args) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).expect("serialize")
        );
    } else if rows.is_empty() {
        println!("no open tracking issues with a merge request");
    } else {
        println!("{}", render(&rows, args.apply));
        let pending = rows
            .iter()
            .filter(|r| {
                !r.assessment.announce.is_empty()
                    || r.assessment.tell_issue_behind
                    || matches!(r.assessment.action, Action::Ping | Action::RebaseMr)
            })
            .count();
        if pending > 0 && !args.apply {
            eprintln!("\n{pending} change(s) have notes to post; pass --apply to post them");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        parse_time(Some(s)).unwrap()
    }

    fn note(who: &str, body: &str, when: &str, system: bool) -> Note {
        serde_json::from_value(serde_json::json!({
            "id": 1, "body": body, "system": system,
            "author": {"username": who}, "created_at": when
        }))
        .unwrap()
    }

    const NOW: &str = "2026-09-22T12:00:00Z";
    const ME: &str = "salimma";
    const W: Windows = Windows {
        quiet: 14,
        reping: 30,
    };
    const PU: &str = "PackageKit-1.2.8-9~proposed.el10";

    fn facts<'a>(mr_notes: &'a [Note], updated: &str) -> Facts<'a> {
        Facts {
            mr_state: "opened",
            has_conflicts: false,
            sha: "2b171ab4",
            updated_at: Some(at(updated)),
            mr_notes,
            issue_notes: &[],
            release_nvr: Some(PU),
            testing_nvr: Some(PU),
            stream_nvr: Some("PackageKit-1.2.8-8.el10"),
        }
    }

    #[test]
    fn a_quiet_clean_mr_with_a_current_build_is_pinged_and_announced_everywhere() {
        let a = assess(&facts(&[], "2025-12-14T20:38:23Z"), ME, at(NOW), W);
        assert_eq!(a.action, Action::Ping);
        assert_eq!(a.quiet_days, 281);
        assert_eq!(
            a.announce,
            vec![Announce {
                nvr: PU.into(),
                stage: "release",
                to_mr: true,
                to_issue: true,
            }],
            "the same NVR in both tags is announced as released only"
        );
        let recent = assess(&facts(&[], "2026-09-15T00:00:00Z"), ME, at(NOW), W);
        assert_eq!(recent.action, Action::Active);
        assert_eq!(recent.quiet_days, 7);
    }

    #[test]
    fn a_build_behind_stock_is_told_to_the_tracking_issue_once_and_never_announced() {
        let mut f = facts(&[], "2025-12-14T20:38:23Z");
        f.stream_nvr = Some("PackageKit-1.2.8-9.el10");
        let a = assess(&f, ME, at(NOW), W);
        assert_eq!(a.action, Action::RebaseBuild);
        assert!(a.tell_issue_behind);
        assert!(a.announce.is_empty(), "no announcing a superseded build");
        let said = [note(
            ME,
            &behind_body("PackageKit-1.2.8-9.el10", PU, "c10s"),
            "2026-09-20T00:00:00Z",
            false,
        )];
        f.issue_notes = &said;
        assert!(!assess(&f, ME, at(NOW), W).tell_issue_behind);
        f.stream_nvr = Some("PackageKit-1.2.8-10.el10");
        assert!(
            assess(&f, ME, at(NOW), W).tell_issue_behind,
            "stock moving again is news again"
        );
    }

    #[test]
    fn a_testing_build_is_announced_as_testing_then_as_released() {
        let mut f = facts(&[], "2026-09-20T00:00:00Z");
        f.release_nvr = None;
        f.testing_nvr = Some("PackageKit-1.2.8-10~proposed.el10");
        let a = assess(&f, ME, at(NOW), W);
        assert_eq!(a.announce.len(), 1);
        assert_eq!(a.announce[0].stage, "testing");
        // Announced in testing on the MR only; the issue still needs it.
        let said = [note(
            ME,
            &announce_body(
                "PackageKit-1.2.8-10~proposed.el10",
                "testing",
                "PackageKit",
                "c10s",
            ),
            "2026-09-21T00:00:00Z",
            false,
        )];
        f.mr_notes = &said;
        let a = assess(&f, ME, at(NOW), W);
        assert_eq!((a.announce[0].to_mr, a.announce[0].to_issue), (false, true));
        // Then it is released: a fresh announcement at the new stage,
        // and the testing one is dropped rather than repeated.
        f.release_nvr = Some("PackageKit-1.2.8-10~proposed.el10");
        let a = assess(&f, ME, at(NOW), W);
        assert_eq!(a.announce.len(), 1);
        assert_eq!(a.announce[0].stage, "release");
        assert!(a.announce[0].to_mr && a.announce[0].to_issue);
    }

    #[test]
    fn conflicts_ask_the_author_once_per_revision_and_hold_the_ping() {
        let mut f = facts(&[], "2025-12-14T20:38:23Z");
        f.has_conflicts = true;
        assert_eq!(assess(&f, ME, at(NOW), W).action, Action::RebaseMr);
        let said = [note(
            ME,
            &rebase_body("ngompa", "c10s", "2b171ab4"),
            "2026-09-01T00:00:00Z",
            false,
        )];
        f.mr_notes = &said;
        assert_eq!(
            assess(&f, ME, at(NOW), W).action,
            Action::Active,
            "said already, and no ping while it conflicts"
        );
        f.sha = "deadbeef";
        assert_eq!(
            assess(&f, ME, at(NOW), W).action,
            Action::RebaseMr,
            "a new revision that still conflicts is told again"
        );
    }

    #[test]
    fn an_unanswered_ping_waits_then_repeats_after_the_reping_window() {
        let ping = |when: &str| {
            note(
                ME,
                &ping_body(at("2025-12-14T00:00:00Z"), 200, "https://t"),
                when,
                false,
            )
        };
        let fresh = [ping("2026-09-01T00:00:00Z")];
        let a = assess(&facts(&fresh, "2026-09-01T00:00:00Z"), ME, at(NOW), W);
        assert_eq!(
            a.action,
            Action::Waiting,
            "21 days since the ping, under 30"
        );
        let stale = [ping("2026-08-01T00:00:00Z")];
        let a = assess(&facts(&stale, "2026-08-01T00:00:00Z"), ME, at(NOW), W);
        assert_eq!(a.action, Action::Ping, "52 days since the ping: ask again");
        assert_eq!(a.pinged_at, Some(at("2026-08-01T00:00:00Z")));
        let answered = [
            ping("2026-07-01T00:00:00Z"),
            note(
                "maintainer",
                "Sorry, rebasing this week.\nMore below.",
                "2026-07-03T00:00:00Z",
                false,
            ),
        ];
        let a = assess(&facts(&answered, "2026-07-03T00:00:00Z"), ME, at(NOW), W);
        assert_eq!(a.action, Action::Respond);
        assert_eq!(
            a.last_response,
            Some(Response {
                by: "maintainer".into(),
                at: at("2026-07-03T00:00:00Z"),
                excerpt: "Sorry, rebasing this week.".into(),
            })
        );
    }

    #[test]
    fn system_notes_do_not_count_and_a_closed_mr_is_left_alone() {
        let sys = [note("bot", "added 1 commit", "2026-09-20T00:00:00Z", true)];
        let a = assess(&facts(&sys, "2025-12-14T00:00:00Z"), ME, at(NOW), W);
        assert_eq!(
            a.action,
            Action::Ping,
            "a system note is not upstream speaking"
        );
        assert_eq!(a.last_response, None);
        let mut f = facts(&sys, "2025-12-14T00:00:00Z");
        f.mr_state = "merged";
        let a = assess(&f, ME, at(NOW), W);
        assert_eq!(a.action, Action::Closed);
        assert!(a.announce.is_empty());
    }

    #[test]
    fn note_bodies_carry_their_markers() {
        let p = ping_body(
            at("2025-12-14T20:38:23Z"),
            281,
            "https://gitlab.com/CentOS/proposed_updates/rpms/PackageKit/-/work_items/3",
        );
        assert!(
            p.contains("no activity since 2025-12-14 (281 days)") && p.contains("work_items/3")
        );
        assert!(p.ends_with(&format!("{PING_MARKER}\n")));
        let r = rebase_body("ngompa", "c10s", "2b171ab4");
        assert!(
            r.starts_with("@ngompa:") && r.ends_with("<!-- cpu-sig-tracker: rebase 2b171ab4 -->\n")
        );
        let b = announce_body(PU, "release", "PackageKit", "c10s");
        assert!(b.contains("released `PackageKit-1.2.8-9~proposed.el10` for CentOS Stream 10"));
        assert!(b.contains("dnf update PackageKit"));
        assert!(b.ends_with(&format!("<!-- cpu-sig-tracker: build {PU} release -->\n")));
        let t = announce_body(PU, "testing", "PackageKit", "c10s");
        assert!(t.contains("buildlogs.centos.org/centos/10-stream/proposed_updates/"));
        assert!(t.ends_with(&format!("<!-- cpu-sig-tracker: build {PU} testing -->\n")));
        let h = behind_body("PackageKit-1.2.8-9.el10", PU, "c10s");
        assert!(h.starts_with("Stock CentOS Stream 10 now carries `PackageKit-1.2.8-9.el10`"));
        assert!(h.ends_with("<!-- cpu-sig-tracker: behind PackageKit-1.2.8-9.el10 -->\n"));
    }

    #[test]
    fn render_names_the_action_the_owner_and_the_last_response() {
        let row = Row {
            release: "c10s".into(),
            package: "PackageKit".into(),
            issue_url: "https://t/3".into(),
            mr_url: "https://gitlab.com/redhat/centos-stream/rpms/PackageKit/-/merge_requests/9"
                .into(),
            mr_state: "opened".into(),
            mr_author: "ngompa".into(),
            release_nvr: Some(PU.into()),
            testing_nvr: Some(PU.into()),
            stream_nvr: Some("PackageKit-1.2.8-9.el10".into()),
            assessment: Assessment {
                action: Action::RebaseBuild,
                tell_issue_behind: true,
                announce: vec![],
                last_activity: at("2025-12-14T20:38:23Z"),
                quiet_days: 281,
                pinged_at: None,
                last_response: Some(Response {
                    by: "maintainer".into(),
                    at: at("2026-07-03T00:00:00Z"),
                    excerpt: "Sorry, rebasing this week.".into(),
                }),
            },
            posted: vec![],
        };
        let text = render(&[row], false);
        assert!(
            text.contains(
                "rebase-build — SIG: stock PackageKit-1.2.8-9.el10 is past \
                 PackageKit-1.2.8-9~proposed.el10; would post note on the tracking issue; \
                 quiet 281 days (since 2025-12-14)"
            ),
            "{text}"
        );
        assert!(
            text.ends_with(
                "    last response 2026-07-03 by maintainer: Sorry, rebasing this week."
            ),
            "{text}"
        );
    }
}
