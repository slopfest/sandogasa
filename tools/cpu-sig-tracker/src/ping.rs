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
//! Before any of that, stock Stream carrying the SIG's build as-is
//! means the change **landed**: nothing to nudge, `retire`. A merged
//! or closed MR means upstream took the change or dropped it: the
//! report says who and when, and whether stock is past the SIG's
//! build. A tracking issue with **no MR** yet still gets its builds
//! announced. A ping is held while GitLab has not checked the MR's
//! mergeability (its `detailed_merge_status`), so an old MR is not
//! nudged before it is known to merge; GitLab is asked to recheck on
//! every read.
//!
//! When someone upstream spoke last the SIG owes the reply, so the
//! last response is shown instead. Nothing is posted unasked: at a
//! terminal each note is offered one by one, `--apply` posts them all
//! for an unattended run, and otherwise the run only reports. Every
//! note carries a hidden marker so later runs recognise it.

use std::process::ExitCode;

use chrono::{DateTime, Utc};

use crate::gitlab::{self, Note};
use crate::status::{
    fetch_proposed_updates_nvrs, fetch_proposed_updates_testing_nvrs, fetch_stream_nvrs,
    parse_mr_line, scan_mr_url_in_body, stream_carries_proposed, stream_newer_than_proposed,
    tracking_project_of,
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

    /// Take the default answer at every prompt without asking, for
    /// an unattended run: post each note unless the SIG owes a reply
    /// on that change
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Report only: never prompt, never post
    #[arg(long)]
    pub dry_run: bool,

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
    /// Stock Stream carries the SIG's build as-is: the change landed,
    /// and the Proposed Update is done.
    Landed,
    /// Stock Stream is past the SIG's build: the SIG rebuilds first.
    RebaseBuild,
    /// The tracking issue names no merge request yet: nothing
    /// upstream to nudge, builds still announced on the issue.
    NoMr,
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
    /// A ping is due but GitLab has not checked whether the MR still
    /// merges (`unchecked`/`checking`): held until it has.
    MergeUnknown,
    /// Merged or closed upstream.
    Closed,
}

/// The last thing a person other than us wrote on the MR.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Response {
    pub by: String,
    pub at: DateTime<Utc>,
    /// The note's text flattened onto one line, cut at about 120
    /// characters — a greeting on the first line is not the message.
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
    /// `opened`, `merged`, `closed`; `none` when the tracking issue
    /// names no MR.
    pub mr_state: &'a str,
    pub has_conflicts: bool,
    /// GitLab's `detailed_merge_status`: `mergeable`, `conflict`,
    /// `need_rebase`, `unchecked`, `checking`, …
    pub merge_status: &'a str,
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
    /// Empty when the tracking issue names no MR.
    pub mr_url: String,
    pub mr_state: String,
    pub mr_author: String,
    /// GitLab's `detailed_merge_status` for an open MR, after a
    /// recheck: `mergeable`, `not_approved`, `conflict`, …
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mr_merge_status: Option<String>,
    /// Who merged or closed the MR, when it is not open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mr_closed_by: Option<String>,
    /// When the MR was merged or closed (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mr_closed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_nvr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub testing_nvr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_nvr: Option<String>,
    #[serde(flatten)]
    pub assessment: Assessment,
    /// Notes posted on this run, as `what@where`.
    pub posted: Vec<String>,
    /// Notes this run would post, until [`post`] decides.
    #[serde(skip)]
    pub pending: Vec<Pending>,
}

/// A note to post, where, and the `what@where` tag it is reported as.
#[derive(Debug, Clone)]
pub struct Pending {
    /// `what@where`, as the JSON `posted` list reports it.
    pub tag: String,
    /// The same for a person: "the release announcement of
    /// blktrace-1.2.0-21~proposed.el9 on the MR".
    pub describe: String,
    pub target: Target,
    pub body: String,
}

/// Where a note goes.
#[derive(Debug, Clone)]
pub enum Target {
    Mr { project: String, iid: u64 },
    Issue { project: String, iid: u64 },
}

fn author(n: &Note) -> &str {
    n.author.as_ref().map_or("", |a| a.username.as_str())
}

fn parse_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// A note's text on one line, markers and blank lines dropped, cut at
/// about 120 characters.
fn excerpt(body: &str) -> String {
    let flat = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("<!--"))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    match flat.char_indices().nth(120) {
        Some((cut, _)) => format!("{}…", flat[..cut].trim_end()),
        None => flat,
    }
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
        excerpt: excerpt(&n.body),
    });
    let ours_last = human.last().is_some_and(|n| author(n) == me);
    let landed = stream_carries_proposed(f.release_nvr, f.stream_nvr);
    let build_behind = !landed && stream_newer_than_proposed(f.release_nvr, f.stream_nvr);
    let rebase_said = ours(f.mr_notes, me, &format!("{REBASE_MARKER}{} -->", f.sha)).is_some();
    let ping_due = match pinged_at {
        None => true,
        Some(t) => (now - t).num_days() >= w.reping,
    };
    // `has_conflicts` is only as fresh as GitLab's last check; the
    // detailed status says when that check has not happened.
    let unmergeable = f.has_conflicts || matches!(f.merge_status, "conflict" | "need_rebase");
    let unchecked = matches!(f.merge_status, "unchecked" | "checking");
    // Landed outranks everything; a merged or closed MR outranks the
    // build comparison, since upstream took the change or dropped it
    // and the SIG has nothing to rebuild for it.
    let action = if landed {
        Action::Landed
    } else if !matches!(f.mr_state, "opened" | "none") {
        Action::Closed
    } else if build_behind {
        Action::RebaseBuild
    } else if f.mr_state == "none" {
        Action::NoMr
    } else if unmergeable && !rebase_said {
        Action::RebaseMr
    } else if last_other.is_some() && !ours_last {
        Action::Respond
    } else if quiet_days < w.quiet || unmergeable {
        Action::Active
    } else if !ping_due {
        Action::Waiting
    } else if unchecked {
        Action::MergeUnknown
    } else {
        Action::Ping
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
    if matches!(f.mr_state, "opened" | "none") && !build_behind && !landed {
        let stages = [(f.release_nvr, "release"), (f.testing_nvr, "testing")];
        for (nvr, stage) in stages {
            let Some(nvr) = nvr else { continue };
            if stage == "testing" && f.release_nvr == Some(nvr) {
                continue;
            }
            let marker = build_marker(nvr, stage);
            let to_mr = f.mr_state == "opened" && ours(f.mr_notes, me, &marker).is_none();
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
            let Some(tracking_project) = tracking_project_of(&issue.web_url) else {
                eprintln!("warning: {package} {release}: unrecognised issue URL; skipped");
                continue;
            };
            let issue_client = gitlab::client(&base, &tracking_project)?;
            let issue_notes = issue_client.issue_notes(issue.iid)?;
            // An issue without an MR is still a change in flight: its
            // builds are announced on the issue, and stock is compared.
            let upstream = match parse_mr_line(&body)
                .map(|(u, _)| u)
                .or_else(|| scan_mr_url_in_body(&body))
            {
                Some(mr_url) => {
                    let (_, project, iid) = gitlab::parse_mr_url(&mr_url)?;
                    if args.verbose {
                        eprintln!("[ping] {package} {release}: reading {project}!{iid}");
                    }
                    let mr_client = gitlab::client(&base, &project)?;
                    let mr = mr_client.merge_request_rechecked(iid)?;
                    let notes = mr_client.merge_request_notes(iid)?;
                    Some((mr_url, project, iid, mr, notes))
                }
                None => None,
            };
            let (mr_url, project, iid, mr, mr_notes) = match &upstream {
                Some((u, p, i, m, n)) => (u.clone(), Some(p.clone()), *i, Some(m), n.as_slice()),
                None => (String::new(), None, 0, None, &[][..]),
            };
            let mr_author = mr
                .and_then(|m| m.author.as_ref())
                .map(|a| a.username.clone())
                .unwrap_or_default();
            let facts = Facts {
                mr_state: mr.map_or("none", |m| m.state.as_str()),
                has_conflicts: mr.and_then(|m| m.has_conflicts).unwrap_or(false),
                merge_status: mr
                    .and_then(|m| m.detailed_merge_status.as_deref())
                    .unwrap_or(""),
                sha: mr.and_then(|m| m.sha.as_deref()).unwrap_or(""),
                updated_at: mr.and_then(|m| parse_time(m.updated_at.as_deref())),
                mr_notes,
                issue_notes: &issue_notes,
                release_nvr: release_nvrs.get(&package).map(String::as_str),
                testing_nvr: testing_nvrs.get(&package).map(String::as_str),
                stream_nvr: stream_nvrs.get(&package).map(String::as_str),
            };
            let assessment = assess(&facts, &me, now, windows);
            let on_mr = |tag: &str, what: &str, body: String| Pending {
                tag: tag.to_string(),
                describe: format!("{what} on the MR"),
                target: Target::Mr {
                    project: project.clone().unwrap_or_default(),
                    iid,
                },
                body,
            };
            let on_issue = |tag: &str, what: &str, body: String| Pending {
                tag: tag.to_string(),
                describe: format!("{what} on the tracking issue"),
                target: Target::Issue {
                    project: tracking_project.clone(),
                    iid: issue.iid,
                },
                body,
            };
            let mut pending = Vec::new();
            for a in &assessment.announce {
                let text = announce_body(&a.nvr, a.stage, &package, release);
                let what = format!("the {} announcement of {}", a.stage, a.nvr);
                if a.to_mr {
                    pending.push(on_mr(
                        &format!("announce {}@mr", a.stage),
                        &what,
                        text.clone(),
                    ));
                }
                if a.to_issue {
                    pending.push(on_issue(
                        &format!("announce {}@issue", a.stage),
                        &what,
                        text,
                    ));
                }
            }
            match assessment.action {
                Action::RebaseBuild if assessment.tell_issue_behind => pending.push(on_issue(
                    "behind@issue",
                    &format!(
                        "the note that stock {} is past the SIG's build",
                        facts.stream_nvr.unwrap_or("")
                    ),
                    behind_body(
                        facts.stream_nvr.unwrap_or(""),
                        facts.release_nvr.unwrap_or(""),
                        release,
                    ),
                )),
                Action::RebaseMr => pending.push(on_mr(
                    "rebase@mr",
                    &format!("the rebase request to @{mr_author}"),
                    rebase_body(
                        &mr_author,
                        mr.map_or("", |m| m.target_branch.as_str()),
                        facts.sha,
                    ),
                )),
                Action::Ping => pending.push(on_mr(
                    "ping@mr",
                    "the ping to the maintainer",
                    ping_body(
                        assessment.last_activity,
                        assessment.quiet_days,
                        &issue.web_url,
                    ),
                )),
                _ => {}
            }
            rows.push(Row {
                release: release.clone(),
                package: package.clone(),
                issue_url: issue.web_url.clone(),
                mr_url,
                mr_state: facts.mr_state.to_string(),
                mr_author,
                mr_merge_status: mr
                    .filter(|m| m.state == "opened")
                    .and_then(|m| m.detailed_merge_status.clone()),
                mr_closed_by: mr.and_then(|m| {
                    m.merged_by
                        .as_ref()
                        .or(m.closed_by.as_ref())
                        .map(|u| u.username.clone())
                }),
                mr_closed_at: mr.and_then(|m| m.merged_at.clone().or(m.closed_at.clone())),
                release_nvr: facts.release_nvr.map(str::to_string),
                testing_nvr: facts.testing_nvr.map(str::to_string),
                stream_nvr: facts.stream_nvr.map(str::to_string),
                assessment,
                posted: Vec::new(),
                pending,
            });
        }
    }
    rows.sort_by(|a, b| (&a.release, &a.package).cmp(&(&b.release, &b.package)));
    Ok(rows)
}

/// The default answer to "post this note?": yes, unless someone
/// upstream is waiting on the SIG — a note landing on top of an
/// unanswered question reads as ignoring it, so the reply comes first.
fn default_answer(action: Action) -> bool {
    action != Action::Respond
}

/// Post the pending notes: at a terminal each one after asking, with
/// [`default_answer`] as the default; with `-y` the default answer
/// unasked; none otherwise — an unattended run must not write
/// unasked. Returns how many were left unposted.
fn post(rows: &mut [Row], args: &PingArgs) -> Result<usize, Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    let base = gitlab_base();
    let interactive = !args.json && std::io::stdin().is_terminal();
    let mut skipped = 0;
    for r in rows.iter_mut() {
        let default = default_answer(r.assessment.action);
        for p in std::mem::take(&mut r.pending) {
            let go = if args.yes {
                default
            } else if interactive {
                let owed = if default {
                    ""
                } else {
                    " — the SIG owes a reply on this change first"
                };
                sandogasa_cli::confirm(
                    &format!("{} {}: post {}?{owed}", r.package, r.release, p.describe),
                    default,
                )?
            } else {
                false
            };
            if !go {
                skipped += 1;
                continue;
            }
            match &p.target {
                Target::Mr { project, iid } => {
                    gitlab::client(&base, project)?.add_merge_request_note(*iid, &p.body)?
                }
                Target::Issue { project, iid } => {
                    gitlab::client(&base, project)?.add_note(*iid, &p.body)?
                }
            }
            if !args.json {
                println!("{} {}: posted {}", r.package, r.release, p.describe);
            }
            r.posted.push(p.tag);
        }
    }
    Ok(skipped)
}

/// What a merged or closed MR means for the SIG: upstream has taken
/// the change, or dropped it, and nothing is left to nudge. What is
/// left is to verify the change is in stock — the build comparison
/// says so when stock is past the SIG's build; otherwise a person
/// checks — and then to `retire` the update.
fn closed_line(r: &Row) -> String {
    let who = r
        .mr_closed_by
        .as_deref()
        .map(|u| format!(" by @{u}"))
        .unwrap_or_default();
    let when = r
        .mr_closed_at
        .as_deref()
        .and_then(|t| parse_time(Some(t)))
        .map(|t| format!(" on {}", t.format("%Y-%m-%d")))
        .unwrap_or_default();
    let sig = r.release_nvr.as_deref().unwrap_or("no release build");
    let verdict = match r.stream_nvr.as_deref() {
        Some(stock) if stream_newer_than_proposed(r.release_nvr.as_deref(), Some(stock)) => {
            format!(
                "stock {} has {stock}, past the SIG's {sig} — verify the change is in, then `retire`",
                r.release
            )
        }
        Some(stock) => format!(
            "stock {} has {stock}, not past the SIG's {sig} — check whether the change landed \
             another way (a maintainer's own build, say) before `retire`",
            r.release
        ),
        None => format!(
            "stock {} has no build to compare with the SIG's {sig} — verify the change landed, \
             then `retire`",
            r.release
        ),
    };
    format!("{}{who}{when} — {verdict}", r.mr_state)
}

/// One block per change: the action and whose it is, the builds, and
/// the last upstream word when there is one; then, for more than one
/// change, what the SIG has to do next, gathered.
pub fn render(rows: &[Row]) -> String {
    let verb = |what: &str, done: bool| {
        if done {
            format!("posted {what}")
        } else {
            format!("to post {what}")
        }
    };
    let mut out = Vec::new();
    for r in rows {
        let a = &r.assessment;
        let has = |p: &str| r.posted.iter().any(|x| x == p);
        let what = match a.action {
            Action::Landed => format!(
                "landed — stock {} carries {}, the SIG's build as-is: `retire`",
                r.release,
                r.stream_nvr.as_deref().unwrap_or("?")
            ),
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
            Action::NoMr => "no MR yet — nothing upstream to nudge".to_string(),
            Action::RebaseMr => format!(
                "rebase-mr — @{}: conflicts; {}",
                r.mr_author,
                verb("rebase note", has("rebase@mr"))
            ),
            Action::Respond => {
                "respond — SIG owes the reply; notes here default to no until it is given"
                    .to_string()
            }
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
            Action::MergeUnknown => {
                "ping held — GitLab has not checked whether this still merges; re-run once it has"
                    .to_string()
            }
            Action::Closed => closed_line(r),
        };
        let quiet = if matches!(a.action, Action::Closed | Action::Landed | Action::NoMr) {
            String::new()
        } else {
            format!(
                "; quiet {} days (since {}){}",
                a.quiet_days,
                a.last_activity.format("%Y-%m-%d"),
                r.mr_merge_status
                    .as_deref()
                    .map(|m| format!("; GitLab: {m}"))
                    .unwrap_or_default()
            )
        };
        // A blank line between changes: two releases of one package
        // read as one block otherwise.
        if !out.is_empty() {
            out.push(String::new());
        }
        let link = if r.mr_url.is_empty() {
            &r.issue_url
        } else {
            &r.mr_url
        };
        out.push(format!(
            "{} {}: {link}\n    {what}{quiet}",
            r.package, r.release
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
    if rows.len() > 1 {
        out.push(String::new());
        out.push(summary(rows));
    }
    out.join("\n")
}

/// What the SIG has to do next, by kind, one line each: the changes
/// to retire, to rebuild, to answer, and how many notes wait.
fn summary(rows: &[Row]) -> String {
    let names = |f: &dyn Fn(&Row) -> bool| {
        rows.iter()
            .filter(|r| f(r))
            .map(|r| format!("{} {}", r.package, r.release))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let retire = names(&|r| {
        r.assessment.action == Action::Landed
            || (r.assessment.action == Action::Closed
                && stream_newer_than_proposed(r.release_nvr.as_deref(), r.stream_nvr.as_deref()))
    });
    let rebuild = names(&|r| r.assessment.action == Action::RebaseBuild);
    let reply = names(&|r| r.assessment.action == Action::Respond);
    let held = names(&|r| r.assessment.action == Action::MergeUnknown);
    let notes = rows
        .iter()
        .map(|r| r.pending.len() + r.posted.len())
        .sum::<usize>();
    let behind_reply = rows
        .iter()
        .filter(|r| !default_answer(r.assessment.action))
        .map(|r| r.pending.len())
        .sum::<usize>();
    let mut out = vec!["Next for the SIG:".to_string()];
    for (label, list) in [
        ("retire (landed, or upstream took over)", retire),
        ("rebuild", rebuild),
        ("reply owed", reply),
        ("ping held until GitLab rechecks", held),
    ] {
        if !list.is_empty() {
            out.push(format!("  {label}: {list}"));
        }
    }
    out.push(match behind_reply {
        0 => format!("  notes: {notes}"),
        n => format!("  notes: {notes}, {n} behind a reply the SIG owes"),
    });
    out.join("\n")
}

pub fn run(args: &PingArgs) -> ExitCode {
    let mut rows = match scan(args) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.json {
        // Post first (the defaults, with -y only), so the rows say
        // what went out.
        if !args.dry_run
            && let Err(e) = post(&mut rows, args)
        {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).expect("serialize")
        );
        return ExitCode::SUCCESS;
    }
    if rows.is_empty() {
        println!("no open tracking issues");
        return ExitCode::SUCCESS;
    }
    println!("{}", render(&rows));
    let pending: usize = rows.iter().map(|r| r.pending.len()).sum();
    if pending == 0 {
        return ExitCode::SUCCESS;
    }
    if args.dry_run {
        eprintln!("\n{pending} note(s) would be offered; run without --dry-run to be asked");
        return ExitCode::SUCCESS;
    }
    println!();
    match post(&mut rows, args) {
        Ok(0) => {}
        Ok(skipped) if args.yes => eprintln!("{skipped} note(s) not posted (reply owed)"),
        Ok(skipped) => eprintln!(
            "{skipped} note(s) left unposted; at a terminal each is offered, -y takes the defaults"
        ),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
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
            merge_status: "mergeable",
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
        f.stream_nvr = Some("PackageKit-1.2.8-10.el10");
        let a = assess(&f, ME, at(NOW), W);
        assert_eq!(a.action, Action::RebaseBuild);
        assert!(a.tell_issue_behind);
        assert!(a.announce.is_empty(), "no announcing a superseded build");
        let said = [note(
            ME,
            &behind_body("PackageKit-1.2.8-10.el10", PU, "c10s"),
            "2026-09-20T00:00:00Z",
            false,
        )];
        f.issue_notes = &said;
        assert!(!assess(&f, ME, at(NOW), W).tell_issue_behind);
        f.stream_nvr = Some("PackageKit-1.2.8-11.el10");
        assert!(
            assess(&f, ME, at(NOW), W).tell_issue_behind,
            "stock moving again is news again"
        );
    }

    #[test]
    fn the_sigs_build_in_stock_as_is_has_landed_whatever_the_mr_says() {
        // Stock carries PU minus `~proposed`: the change is in, and
        // that outranks a closed MR, a quiet one, and any announcing.
        for state in ["opened", "closed", "none"] {
            let mut f = facts(&[], "2025-12-14T20:38:23Z");
            f.mr_state = state;
            f.stream_nvr = Some("PackageKit-1.2.8-9.el10");
            let a = assess(&f, ME, at(NOW), W);
            assert_eq!(a.action, Action::Landed, "{state}");
            assert!(a.announce.is_empty(), "{state}");
            assert!(!a.tell_issue_behind, "{state}");
        }
    }

    #[test]
    fn an_issue_without_an_mr_announces_its_builds_on_the_issue_only() {
        let mut f = facts(&[], "2025-12-14T20:38:23Z");
        f.mr_state = "none";
        let a = assess(&f, ME, at(NOW), W);
        assert_eq!(a.action, Action::NoMr);
        assert_eq!(a.announce.len(), 1);
        assert!(!a.announce[0].to_mr && a.announce[0].to_issue);
    }

    #[test]
    fn a_ping_waits_for_gitlab_to_check_mergeability_and_a_stale_conflict_is_a_rebase() {
        let mut f = facts(&[], "2025-12-14T20:38:23Z");
        f.merge_status = "unchecked";
        assert_eq!(assess(&f, ME, at(NOW), W).action, Action::MergeUnknown);
        f.merge_status = "checking";
        assert_eq!(assess(&f, ME, at(NOW), W).action, Action::MergeUnknown);
        // A conflict GitLab reports in the detailed status counts even
        // when `has_conflicts` still says false.
        f.merge_status = "need_rebase";
        assert_eq!(assess(&f, ME, at(NOW), W).action, Action::RebaseMr);
        f.merge_status = "mergeable";
        assert_eq!(assess(&f, ME, at(NOW), W).action, Action::Ping);
    }

    #[test]
    fn a_reply_owed_turns_the_default_answer_to_no() {
        assert!(!default_answer(Action::Respond));
        for a in [
            Action::Ping,
            Action::RebaseMr,
            Action::RebaseBuild,
            Action::NoMr,
        ] {
            assert!(default_answer(a), "{a:?}");
        }
    }

    #[test]
    fn an_excerpt_is_the_message_not_its_greeting() {
        let body = "Hi @michel-slm ,\n\nCould you rebase this onto the current branch? \
                    The spec moved.\n\n<!-- cpu-sig-tracker: ping -->\n";
        assert_eq!(
            excerpt(body),
            "Hi @michel-slm , Could you rebase this onto the current branch? The spec moved."
        );
        let long = "word ".repeat(60);
        let cut = excerpt(&long);
        assert!(cut.ends_with('…') && cut.chars().count() <= 121, "{cut}");
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
                excerpt: "Sorry, rebasing this week. More below.".into(),
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
            mr_merge_status: Some("mergeable".into()),
            mr_closed_by: None,
            mr_closed_at: None,
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
            pending: vec![],
        };
        let text = render(&[row]);
        assert!(
            text.contains(
                "rebase-build — SIG: stock PackageKit-1.2.8-9.el10 is past \
                 PackageKit-1.2.8-9~proposed.el10; to post note on the tracking issue; \
                 quiet 281 days (since 2025-12-14); GitLab: mergeable"
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

    #[test]
    fn a_closed_mr_says_who_closed_it_and_points_at_retire() {
        let sys = [note("bot", "closed", "2026-04-27T00:00:00Z", true)];
        let row = |stream_nvr: Option<&str>| {
            let mut f = facts(&sys, "2026-04-27T00:00:00Z");
            f.mr_state = "closed";
            f.release_nvr = Some("PackageKit-1.2.8-9~proposed.el10");
            f.stream_nvr = stream_nvr;
            Row {
                release: "c10s".into(),
                package: "PackageKit".into(),
                issue_url: String::new(),
                mr_url: "https://gitlab.example/mr/13".into(),
                mr_state: "closed".into(),
                mr_author: "ngompa".into(),
                mr_merge_status: None,
                mr_closed_by: Some("hughsie".into()),
                mr_closed_at: Some("2026-04-27T10:00:00Z".into()),
                release_nvr: f.release_nvr.map(str::to_string),
                testing_nvr: None,
                stream_nvr: stream_nvr.map(str::to_string),
                assessment: assess(&f, ME, at(NOW), W),
                posted: vec![],
                pending: vec![],
            }
        };
        // Stock past the SIG's build: verify, then retire.
        let out = render(&[row(Some("PackageKit-1.2.8-10.el10"))]);
        assert!(out.contains("closed by @hughsie on 2026-04-27"), "{out}");
        assert!(
            out.contains("past the SIG's PackageKit-1.2.8-9~proposed.el10"),
            "{out}"
        );
        assert!(out.contains("then `retire`"), "{out}");
        assert!(
            !out.contains("quiet"),
            "a closed MR has nothing to be quiet about: {out}"
        );
        // Stock not there: a person checks how the change landed.
        let out = render(&[row(Some("PackageKit-1.2.8-8.el10"))]);
        assert!(
            out.contains("not past the SIG's") && out.contains("another way"),
            "{out}"
        );
        let out = render(&[row(None)]);
        assert!(out.contains("no build to compare"), "{out}");
    }
}
