// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `retire` subcommand.
//!
//! Closes a tracking issue classified as `retire-issue` by
//! `status` — i.e. one where JIRA has been resolved and the
//! package is no longer tagged in the proposed_updates
//! `-release` Koji tag, so the tracking issue is just leftover
//! bookkeeping.
//!
//! Safety-first flow: fetch the issue, verify both conditions,
//! prompt the user, leave an audit-trail comment, then close.
//! `--force` skips the condition checks, `--yes` skips the
//! prompt.

use std::process::ExitCode;

use sandogasa_koji::{list_tagged_nvrs, parse_nvr_name};

use crate::dump_inventory::proposed_updates_tag;
use crate::gitlab;
use crate::ping::{evidence_tokens, find_evidence};
use crate::status::{
    fetch_proposed_updates_nvrs, fetch_stream_nvrs, parse_mr_line, scan_mr_url_in_body,
    stream_newer_than_proposed,
};
use crate::utils::{Check, parse_jira_key_from_body, report_check};

/// Why a Proposed Update is retired — the one question the operator
/// answers: is the problem fixed in stock, or is the SIG giving up?
/// How it got fixed is read from the evidence and becomes the label
/// (see [`Reason::label`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Reason {
    /// The problem is fixed in stock — by the SIG's MR merging, or
    /// another way (labelled `superseded`).
    Landed,
    /// The SIG gives up on the change (Bugzilla's CANTFIX).
    Abandoned,
}

impl Reason {
    /// The label put on the tracking issue: `landed` when the SIG's
    /// own MR merged, `superseded` when the fix reached stock another
    /// way — a stock commit naming it, the RHEL issue resolved, or the
    /// operator's own verification — and `abandoned`.
    pub fn label(self, mr_merged: bool) -> &'static str {
        match self {
            Reason::Landed if mr_merged => "landed",
            Reason::Landed => "superseded",
            Reason::Abandoned => "abandoned",
        }
    }

    /// The GitLab work-item status the issue closes with.
    pub fn work_item_status(self) -> &'static str {
        match self {
            Reason::Landed => "Done",
            Reason::Abandoned => "Won't do",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "landed" | "fixed" | "l" | "f" => Some(Reason::Landed),
            "abandoned" | "a" => Some(Reason::Abandoned),
            _ => None,
        }
    }
}

/// What is known about the change when it is retired, the material
/// the reason is inferred from and checked against.
#[derive(Debug, Default)]
pub struct RetireFacts {
    /// The upstream MR the tracking issue names, and its state as
    /// GitLab reports it (`None` when it could not be read).
    pub mr_url: Option<String>,
    pub mr_state: Option<String>,
    /// The SIG's `-release` build and stock's build.
    pub release_nvr: Option<String>,
    pub stream_nvr: Option<String>,
    /// Why the problem counts as fixed in stock, when something says
    /// so: `MR merged`, a stock commit naming the change, the RHEL
    /// issue resolved as Done, or the operator's own verification.
    pub landed_by: Option<String>,
    /// The RHEL issue's resolution, when it is resolved.
    pub jira_resolution: Option<String>,
    /// The stock build a person named as carrying the fix, to record
    /// on the issue.
    pub recorded_fix: Option<String>,
}

impl RetireFacts {
    pub fn mr_merged(&self) -> bool {
        self.mr_state.as_deref() == Some("merged")
    }

    /// Stock is past the SIG's build with nothing saying the fix is
    /// in — the rebase case, never a reason to retire.
    pub fn stock_past_without_evidence(&self) -> bool {
        self.landed_by.is_none()
            && stream_newer_than_proposed(self.release_nvr.as_deref(), self.stream_nvr.as_deref())
    }
}

/// The reason the facts point at, if they point anywhere: landed on
/// evidence, abandoned when the RHEL issue was closed without a fix.
/// Stock being past the SIG's build points at a rebase, not here.
pub fn suggest_reason(f: &RetireFacts) -> Option<Reason> {
    if f.landed_by.is_some() {
        return Some(Reason::Landed);
    }
    f.jira_resolution.as_deref().map(|_| Reason::Abandoned)
}

/// What has to hold for the reason, beyond the build being untagged:
/// landed wants the evidence (or the operator's word, recorded as
/// such), abandoned nothing more.
pub fn reason_check(reason: Reason, f: &RetireFacts) -> Check {
    match reason {
        Reason::Landed => match &f.landed_by {
            Some(why) => Check::Pass(why.clone()),
            None => Check::Fail(
                "nothing says the fix is in stock: the MR is not merged and no stock commit \
                 names the change (its RHEL key, CVE or title); verify it yourself at the \
                 prompt, or --force"
                    .to_string(),
            ),
        },
        Reason::Abandoned => Check::Pass("the SIG's call; nothing to verify".to_string()),
    }
}

/// The note left on the upstream MR when the SIG retires the change
/// it carries, so its reviewers hear it from the SIG rather than from
/// a closed tracking issue nobody follows.
pub fn mr_note_body(reason: Reason, f: &RetireFacts, issue_url: &str) -> String {
    let what = match reason {
        Reason::Landed => format!(
            "the problem this change addresses is fixed in CentOS Stream now ({}), so the \
             CentOS Proposed Updates SIG has retired its Proposed Update carrying it. This \
             merge request can be closed if it is not already.",
            f.landed_by.as_deref().unwrap_or("fixed in stock")
        ),
        Reason::Abandoned => "the CentOS Proposed Updates SIG is withdrawing this change and \
             closing this merge request with it; the Proposed Update that carried it is \
             retired."
            .to_string(),
    };
    format!(
        "Note from the SIG: {what}\n\nTracking issue: {issue_url}\n\n<!-- cpu-sig-tracker: retired {} -->\n",
        reason.label(f.mr_merged())
    )
}

const KOJI_PROFILE: &str = "cbs";

#[derive(clap::Args)]
pub struct RetireArgs {
    /// Tracking issue URL (`/-/issues/<n>` or `/-/work_items/<n>`),
    /// or a package name, resolved through the SIG's open tracking
    /// issues (`list-issues`).
    pub issue: String,

    /// With a package name: the release to retire it in (e.g.
    /// `c10s`), when it is tracked in several.
    #[arg(long)]
    pub release: Option<String>,

    /// Why: landed (the problem is fixed in stock — labelled
    /// `superseded` when not by the SIG's own MR) or abandoned (the
    /// SIG gives up). Inferred from the MR, stock and the RHEL issue
    /// when omitted, and asked at a terminal.
    #[arg(long, value_enum)]
    pub reason: Option<Reason>,

    /// With `--reason landed` and nothing on record: the stock build
    /// that carries the fix, as you verified it. Recorded on the issue
    /// for `timeline`; asked for at a terminal otherwise.
    #[arg(long, value_name = "NVR", requires = "reason")]
    pub stock_fix: Option<String>,

    /// Skip the interactive confirmation prompt.
    #[arg(short, long)]
    pub yes: bool,

    /// Skip the precondition checks (build untagged, and what the
    /// reason needs). Use when the tool can't reach Koji/GitLab or
    /// when you're sure the conditions hold.
    #[arg(long)]
    pub force: bool,

    /// Also assign the issue to you as it is closed.
    ///
    /// Without this you are asked, unless -y is given: a
    /// non-interactive run must not reassign work nobody asked
    /// it to.
    #[arg(long)]
    pub claim: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

pub fn run(args: &RetireArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_inner(args: &RetireArgs) -> Result<(), Box<dyn std::error::Error>> {
    let issue_url = resolve_issue(args)?;
    let (_parsed_base, project_path, iid) = gitlab::parse_issue_url(&issue_url)?;
    // parse_issue_url extracts the host from the user-supplied
    // URL, but we route API calls through `gitlab_base()` so
    // tests can override via `CPU_SIG_TRACKER_GITLAB_BASE`.
    // In production both are gitlab.com so there's no
    // visible change.
    if args.verbose {
        eprintln!("[cpu-sig-tracker] fetching issue {project_path}!{iid}");
    }
    let client = gitlab::client(&crate::utils::gitlab_base(), &project_path)?;
    let issue = client.issue(iid)?;

    if issue.state == "closed" {
        eprintln!("issue already closed: {}", issue.web_url);
        return Ok(());
    }

    let body = issue.description.as_deref().unwrap_or("");
    // The tool's own issues say the release on a body line; a
    // hand-filed one says it with its label, or the operator does.
    let release = parse_release_from_body(body)
        .or_else(|| args.release.clone())
        .or_else(|| release_from_labels(&issue.labels))
        .ok_or(
            "no release: the issue body has no `- **Release**:` line and no `c<N>s` label; \
             pass --release",
        )?;
    let package = gitlab::package_from_issue_url(&issue.web_url)
        .ok_or("could not derive package name from issue URL")?;
    let jira_key = parse_jira_key_from_body(body);

    // The RHEL issue: informational, and one source of the reason.
    let jira_check = crate::jira::check_resolved(jira_key.as_deref(), args.verbose);
    report_check("JIRA resolved", &jira_check.check);

    // Precondition: no pu build tagged (retire follows untag).
    let build_check = check_package_untagged(&release, package, args.verbose);
    report_check("no pu build tagged", &build_check);

    // What the reason is judged against: the MR, stock and its
    // history, the RHEL resolution.
    let mut facts = gather_facts(&issue, body, package, &release, &jira_check, args.verbose);
    if facts.stock_past_without_evidence() {
        eprintln!(
            "note: stock {} is past the SIG's {} but nothing says the fix is in it — that is \
             the rebase case (`ping` says rebase-build), not a reason to retire",
            facts.stream_nvr.as_deref().unwrap_or("?"),
            facts.release_nvr.as_deref().unwrap_or("?")
        );
    }
    let reason = resolve_reason(args, &facts)?;
    // The fix in stock with nothing on record saying so: the operator
    // can say so — naming the stock build that carries it, which goes
    // on the issue for `timeline` — and the audit note says who did.
    if reason == Reason::Landed && facts.landed_by.is_none() {
        use std::io::IsTerminal;
        let named = match &args.stock_fix {
            Some(nvr) => Some(nvr.clone()),
            None if !args.yes && std::io::stdin().is_terminal() => {
                if sandogasa_cli::confirm(
                    &format!(
                        "Nothing on record says the fix is in stock ({}). Have you verified it is?",
                        facts.stream_nvr.as_deref().unwrap_or("stock build unknown")
                    ),
                    false,
                )? {
                    ask_stock_fix(facts.stream_nvr.as_deref())
                } else {
                    None
                }
            }
            None => None,
        };
        if let Some(nvr) = named {
            facts.landed_by = Some(verified_by_operator(&nvr));
            facts.recorded_fix = Some(nvr);
        }
    }
    let label = reason.label(facts.mr_merged());
    let reason_check = reason_check(reason, &facts);
    report_check(&format!("reason {label}"), &reason_check);

    let preconditions_ok =
        matches!(&build_check, Check::Pass(_)) && matches!(&reason_check, Check::Pass(_));
    if !preconditions_ok && !args.force {
        return Err(
            "retire preconditions not met; re-run with --force to override or fix the \
             underlying state (e.g. run `untag` first if the build is still tagged)"
                .into(),
        );
    }

    // The MR, when it is still open, is told; an abandoned change's
    // MR is the SIG's own and is closed with it.
    let mr_open = facts.mr_state.as_deref() == Some("opened");
    let close_mr = mr_open && reason == Reason::Abandoned;

    println!();
    println!("Will close {}", issue.web_url);
    println!("  title:   {}", issue.title);
    println!("  package: {package}");
    println!("  release: {release}");
    println!("  reason:  {label} (status {})", reason.work_item_status());
    if let Some(url) = &facts.mr_url {
        println!(
            "  MR:      {url} — {}{}",
            facts.mr_state.as_deref().unwrap_or("state unknown"),
            if close_mr {
                "; a note, then closed"
            } else if mr_open {
                "; a note"
            } else {
                ""
            }
        );
    }
    let start_date = derive_start_date(package, &release, &issue, args.verbose);
    if let Some((date, source)) = &start_date {
        println!("  start_date: {date} (from {source})");
    }
    if let Some(date) = jira_check.resolution_date {
        println!("  due_date: {date} (from JIRA resolutiondate)");
    }
    if !args.yes && !sandogasa_cli::confirm("Proceed?", false)? {
        eprintln!("aborted.");
        return Ok(());
    }

    let note = compose_audit_note(
        jira_key.as_deref(),
        &jira_check.check,
        &build_check,
        args.force,
        Some((label, &reason_check)),
    );
    if args.verbose {
        eprintln!("[cpu-sig-tracker] posting audit note");
    }
    client.add_note(iid, &note)?;

    // What a person established goes on the issue, where timeline
    // reads it ahead of any inference.
    if let Some(nvr) = &facts.recorded_fix {
        let who = whoami().unwrap_or_else(|| "the operator".to_string());
        let line = crate::utils::format_stock_fix_line(nvr, &who, chrono::Utc::now().date_naive());
        if let Err(e) = client.edit_issue(
            iid,
            &gitlab::IssueUpdate {
                description: Some(crate::utils::with_stock_fix_line(body, &line)),
                ..Default::default()
            },
        ) {
            eprintln!("warning: could not record the stock fix on the issue: {e}");
        }
    }

    // Flip the work-item status so browsers of the GitLab UI see a
    // terminal state, not just a closed issue: "Done" for a change
    // that made it (landed or superseded), "Won't do" for one given up.
    let terminal_status = reason.work_item_status();
    if args.verbose {
        eprintln!("[cpu-sig-tracker] setting work-item status to {terminal_status}");
    }
    if let Err(e) = client.set_work_item_status(iid, terminal_status) {
        eprintln!("warning: could not set work-item status to {terminal_status}: {e}");
    }

    // Stamp start_date / due_date via GraphQL — REST
    // PUT /issues ignores these for work items.
    let formatted_start = start_date
        .as_ref()
        .map(|(d, _)| d.format("%Y-%m-%d").to_string());
    let formatted_due = jira_check
        .resolution_date
        .map(|d| d.format("%Y-%m-%d").to_string());
    if formatted_start.is_some() || formatted_due.is_some() {
        if args.verbose {
            eprintln!("[cpu-sig-tracker] setting start_date/due_date via GraphQL");
        }
        if let Err(e) =
            client.set_work_item_dates(iid, formatted_start.as_deref(), formatted_due.as_deref())
        {
            eprintln!("warning: could not set start/due dates: {e}");
        }
    }

    // Whether to take the issue as well as close it. Asked after the
    // preconditions and the audit note, so the question only arises for an
    // issue actually being retired.
    let claim = resolve_issue_claim(args)?;
    if args.verbose {
        eprintln!("[cpu-sig-tracker] closing issue");
    }
    let update = gitlab::IssueUpdate {
        state_event: Some("close".to_string()),
        // The reason, as a label a search can find.
        add_labels: Some(label.to_string()),
        // Replaces the assignee set rather than adding to it, which is
        // what claiming means here: a retired issue's owner is whoever
        // retired it.
        assignee_ids: claim.as_ref().map(|(id, _)| vec![*id]),
        ..Default::default()
    };
    client.edit_issue(iid, &update)?;

    match &claim {
        Some((_, who)) => eprintln!("closed {} (assigned to {who})", issue.web_url),
        None => eprintln!("closed {}", issue.web_url),
    }

    // Tell the MR, and close it when the SIG withdraws the change.
    if mr_open && let Some(url) = &facts.mr_url {
        tell_mr(url, reason, &facts, &issue.web_url, close_mr, args.verbose);
    }
    Ok(())
}

/// Read what the reason is inferred from and checked against. Every
/// lookup is best-effort: a MR or a stock history that cannot be read
/// leaves its fact unknown, and the reason then has to come from the
/// operator (or --force).
fn gather_facts(
    issue: &gitlab::Issue,
    body: &str,
    package: &str,
    release: &str,
    jira: &crate::jira::JiraCheck,
    verbose: bool,
) -> RetireFacts {
    let base = crate::utils::gitlab_base();
    let mut f = RetireFacts {
        jira_resolution: jira.resolution_name.clone(),
        ..Default::default()
    };
    f.mr_url = parse_mr_line(body)
        .map(|(u, _)| u)
        .or_else(|| scan_mr_url_in_body(body));
    let mut mr_title = String::new();
    let mut mr_branch = String::new();
    if let Some(url) = &f.mr_url {
        let fetched: Result<sandogasa_gitlab::MergeRequest, Box<dyn std::error::Error>> =
            match gitlab::parse_mr_url(url) {
                Ok((_, project, iid)) => {
                    gitlab::client(&base, &project).and_then(|c| c.merge_request(iid))
                }
                Err(e) => Err(e.into()),
            };
        match fetched {
            Ok(mr) => {
                f.mr_state = Some(mr.state.clone());
                mr_title = mr.title.clone();
                mr_branch = mr.source_branch.clone();
            }
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] cannot read {url}: {e}");
                }
            }
        }
    }
    f.release_nvr = fetch_proposed_updates_nvrs(release, verbose).remove(package);
    f.stream_nvr = fetch_stream_nvrs(release, &[package.to_string()], verbose).remove(package);
    f.landed_by = if f.mr_state.as_deref() == Some("merged") {
        Some("MR merged".to_string())
    } else {
        let tokens = evidence_tokens(body, &issue.title, &mr_title, &mr_branch);
        match gitlab::client(&base, &format!("redhat/centos-stream/rpms/{package}"))
            .and_then(|c| c.branch_commits(release))
        {
            Ok(commits) => find_evidence(&commits, &tokens),
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] cannot read stock history: {e}");
                }
                None
            }
        }
    }
    .or_else(|| match jira.resolution_name.as_deref() {
        Some(r @ ("Done" | "Fixed" | "Resolved")) => Some(format!("RHEL issue resolved {r}")),
        _ => None,
    });
    f
}

/// The reason: `--reason`, else the facts' suggestion — taken as is
/// with `-y`, offered as the default at a terminal — else an error
/// naming the choices, since a run nobody watches must not guess why
/// a change is being dropped.
fn resolve_reason(
    args: &RetireArgs,
    f: &RetireFacts,
) -> Result<Reason, Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    if let Some(r) = args.reason {
        return Ok(r);
    }
    let suggested = suggest_reason(f);
    if args.yes || !std::io::stdin().is_terminal() {
        return suggested.ok_or_else(|| {
            "no reason could be inferred (MR not merged, no stock commit names the change, RHEL \
             issue open); pass --reason landed|abandoned"
                .into()
        });
    }
    ask_reason(suggested, |q| {
        use std::io::{BufRead, Write};
        eprint!("{q}");
        std::io::stderr().flush().ok()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        Some(line)
    })
}

/// Ask for the reason with `suggested` as the default, through `read`
/// (given the prompt, answering the line or `None` on EOF).
fn ask_reason(
    suggested: Option<Reason>,
    read: impl FnOnce(&str) -> Option<String>,
) -> Result<Reason, Box<dyn std::error::Error>> {
    let default = suggested
        .map(|r| format!(", default {}", r.label(true)))
        .unwrap_or_default();
    let q = format!("Why retire it? [landed (fixed in stock)/abandoned{default}]: ");
    let line = read(&q).unwrap_or_default();
    match (Reason::parse(&line), line.trim().is_empty(), suggested) {
        (Some(r), _, _) => Ok(r),
        (None, true, Some(r)) => Ok(r),
        _ => Err("no reason chosen".into()),
    }
}

/// What the audit note says when the operator, not the record,
/// vouches for the fix being in stock.
fn verified_by_operator(nvr: &str) -> String {
    format!(
        "verified by {}: stock {nvr} carries the fix (no commit on record names it)",
        whoami().unwrap_or_else(|| "the operator".to_string())
    )
}

/// The token's GitLab username, when it can be learnt.
fn whoami() -> Option<String> {
    let token = gitlab::load_token().ok()?;
    sandogasa_gitlab::current_user(&crate::utils::gitlab_base(), &token)
        .ok()
        .map(|u| u.username)
}

/// Ask which stock build carries the fix, `default` (stock's current
/// build) on Enter; `None` when nothing is given.
fn ask_stock_fix(default: Option<&str>) -> Option<String> {
    use std::io::{BufRead, Write};
    eprint!(
        "Which stock build carries the fix?{}: ",
        default.map(|d| format!(" [{d}]")).unwrap_or_default()
    );
    std::io::stderr().flush().ok()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let line = line.trim();
    if line.is_empty() {
        default.map(str::to_string)
    } else {
        Some(line.to_string())
    }
}

/// Leave the reason's note on the open MR and, for an abandoned
/// change, close it — the SIG's own MR, withdrawn with the change.
/// Failures warn: the tracking issue is closed already, and the note
/// can be written by hand.
fn tell_mr(
    url: &str,
    reason: Reason,
    f: &RetireFacts,
    issue_url: &str,
    close: bool,
    verbose: bool,
) {
    let Ok((_, project, iid)) = gitlab::parse_mr_url(url) else {
        eprintln!("warning: unrecognised MR URL {url}; nothing posted there");
        return;
    };
    let client = match gitlab::client(&crate::utils::gitlab_base(), &project) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: cannot reach {project}: {e}; nothing posted on the MR");
            return;
        }
    };
    if verbose {
        eprintln!("[cpu-sig-tracker] noting the retirement on {project}!{iid}");
    }
    match client.add_merge_request_note(iid, &mr_note_body(reason, f, issue_url)) {
        Ok(()) => eprintln!("noted on {url}"),
        Err(e) => eprintln!("warning: could not note on {url}: {e}"),
    }
    if close {
        match client.edit_merge_request(
            iid,
            &sandogasa_gitlab::MergeRequestUpdate {
                state_event: Some("close".to_string()),
            },
        ) {
            Ok(_) => eprintln!("closed {url}"),
            Err(e) => eprintln!("warning: could not close {url}: {e}"),
        }
    }
}

/// Whether to assign the issue to the person retiring it, and to whom.
///
/// The decision matrix is [`sandogasa_cli::claim::resolve_claim`], shared
/// with the Bugzilla-side tools so the two cannot drift: `--claim` claims
/// without asking, `-y` alone declines, and otherwise the user is asked.
/// The identity comes from the token rather than from configuration —
/// GitLab assigns by numeric id, which nobody knows offhand, and the token
/// already says who it belongs to.
/// The tracking issue to retire: the URL given, or the one open
/// tracking issue for the package named — in `--release`, or across
/// releases when one match settles it. Several matches are put to
/// the operator at a terminal; unattended, they are an error naming
/// each, since a run nobody watches must not guess which change to
/// close.
fn resolve_issue(args: &RetireArgs) -> Result<String, Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    if args.issue.contains("://") {
        return Ok(args.issue.clone());
    }
    let rows =
        crate::list_issues::tracking_issues(args.release.as_deref(), &args.issue, args.verbose)?;
    let interactive = !args.yes && std::io::stdin().is_terminal();
    pick(&rows, &args.issue, interactive, |n| {
        use std::io::{BufRead, Write};
        eprint!("Which one? [1-{n}, empty to abort]: ");
        std::io::stderr().flush().ok()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).ok()?;
        line.trim().parse::<usize>().ok()
    })
}

/// One row is the answer; none is an error; several are listed and,
/// when `interactive`, chosen by number through `ask` (given the
/// count, answering the 1-based pick or `None` to abort).
fn pick(
    rows: &[crate::list_issues::Row],
    package: &str,
    interactive: bool,
    ask: impl FnOnce(usize) -> Option<usize>,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = |r: &crate::list_issues::Row| r.issue_url.clone().unwrap_or_default();
    match rows {
        [] => Err(format!(
            "no open tracking issue for {package}; `list-issues` shows what is tracked"
        )
        .into()),
        [one] => Ok(url(one)),
        many => {
            let listed: Vec<String> = many
                .iter()
                .enumerate()
                .map(|(i, r)| format!("  {}. {} {}  {}", i + 1, r.package, r.release, url(r)))
                .collect();
            if !interactive {
                return Err(format!(
                    "{package} is tracked in {} releases; pass --release, or the issue URL:\n{}",
                    many.len(),
                    listed.join("\n")
                )
                .into());
            }
            eprintln!("{package} is tracked in {} releases:", many.len());
            for l in &listed {
                eprintln!("{l}");
            }
            match ask(many.len()) {
                Some(n) if (1..=many.len()).contains(&n) => Ok(url(&many[n - 1])),
                _ => Err("no tracking issue chosen".into()),
            }
        }
    }
}

fn resolve_issue_claim(
    args: &RetireArgs,
) -> Result<Option<(u64, String)>, Box<dyn std::error::Error>> {
    // Nothing to ask about, so nothing to look up.
    if !args.claim && args.yes {
        return Ok(None);
    }
    let token = gitlab::load_token()?;
    let me = match sandogasa_gitlab::current_user(&crate::utils::gitlab_base(), &token) {
        Ok(me) => me,
        Err(e) => {
            // Not fatal: the issue is still worth closing, and a claim
            // nobody could resolve is better skipped than guessed at.
            eprintln!("warning: could not learn who you are on GitLab, not claiming: {e}");
            return Ok(None);
        }
    };
    let prompt = format!("Also take this issue (assign it to {})?", me.username);
    let claimed = sandogasa_cli::claim::resolve_claim(
        args.claim,
        args.yes,
        Some(&me.username),
        &prompt,
        |p| sandogasa_cli::confirm(p, false).map_err(|e| e.to_string()),
    )?;
    Ok(claimed.map(|username| (me.id, username)))
}

fn check_package_untagged(release: &str, package: &str, verbose: bool) -> Check {
    let tag = match proposed_updates_tag(release) {
        Ok(t) => t,
        Err(e) => return Check::Skipped(e),
    };
    if verbose {
        eprintln!("[cpu-sig-tracker] listing tagged NVRs in {tag}");
    }
    let nvrs = match list_tagged_nvrs(&tag, Some(KOJI_PROFILE)) {
        Ok(v) => v,
        Err(e) => return Check::Skipped(format!("koji list-tagged {tag} failed: {e}")),
    };
    match nvrs.iter().find(|nvr| parse_nvr_name(nvr) == Some(package)) {
        Some(nvr) => Check::Fail(format!("package still tagged as {nvr} — run `untag` first")),
        None => Check::Pass(format!("no {package} build tagged in {tag}")),
    }
}

fn compose_audit_note(
    jira_key: Option<&str>,
    jira_check: &Check,
    build_check: &Check,
    forced: bool,
    reason: Option<(&str, &Check)>,
) -> String {
    let reason_part = match reason {
        Some((label, c)) => format!(" Reason: {label} — {}.", c.detail()),
        None => String::new(),
    };
    let jira_part = match jira_key {
        Some(k) => format!(" JIRA {k}: {}", jira_check.detail()),
        None => String::new(),
    };
    let build_part = format!(" Build: {}", build_check.detail());
    let forced_part = if forced { " (--force)" } else { "" };
    format!(
        "Closing via `cpu-sig-tracker retire`{forced_part}.{reason_part}{jira_part}{build_part}"
    )
}

/// Best-effort start_date for the tracking issue we're about
/// to close.
///
/// Tries Koji's `-release` / `-testing` tags first (matching
/// `file-issue`'s logic). When the build is no longer tagged
/// — the common case at retire-time, since retirement usually
/// follows untagging — falls back to the issue's own
/// `created_at` timestamp, which is a reasonable approximation
/// of when the SIG started tracking the package.
fn derive_start_date(
    package: &str,
    release: &str,
    issue: &gitlab::Issue,
    verbose: bool,
) -> Option<(chrono::NaiveDate, &'static str)> {
    if let Some(date) = crate::file_issue::find_build_start_date(package, release, verbose) {
        return Some((date, "Koji build creation time"));
    }
    issue
        .created_at
        .as_deref()
        .and_then(crate::utils::parse_iso_date)
        .map(|d| (d, "GitLab issue created_at"))
}

/// The `c<N>s` label among an issue's labels, the release a hand-filed
/// tracking issue names that way.
fn release_from_labels(labels: &[String]) -> Option<String> {
    labels
        .iter()
        .find(|l| {
            l.strip_prefix('c')
                .and_then(|r| r.strip_suffix('s'))
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .cloned()
}

/// Find the `c<N>s` release label in `- **Release**: c10s`.
fn parse_release_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **Release**:") {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(package: &str, release: &str, url: &str) -> crate::list_issues::Row {
        crate::list_issues::Row {
            release: release.into(),
            package: package.into(),
            status: crate::list_issues::TrackingStatus::Active,
            issue_url: Some(url.into()),
            issue_iid: Some(1),
            mr_url: None,
        }
    }

    #[test]
    fn a_package_name_resolves_to_its_one_issue_or_asks_which() {
        let c9 = tracked(
            "blktrace",
            "c9s",
            "https://gitlab.example/rpms/blktrace/-/issues/1",
        );
        let c10 = tracked(
            "blktrace",
            "c10s",
            "https://gitlab.example/rpms/blktrace/-/issues/2",
        );
        // One match: no question asked.
        let url = pick(std::slice::from_ref(&c9), "blktrace", true, |_| {
            panic!("not asked")
        })
        .unwrap();
        assert_eq!(url, "https://gitlab.example/rpms/blktrace/-/issues/1");
        // None: an error pointing at list-issues.
        let err = pick(&[], "nothing", true, |_| None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no open tracking issue for nothing") && err.contains("list-issues"));
        // Several, at a terminal: the pick by number.
        let both = [c9, c10];
        let url = pick(&both, "blktrace", true, |n| {
            assert_eq!(n, 2);
            Some(2)
        })
        .unwrap();
        assert_eq!(url, "https://gitlab.example/rpms/blktrace/-/issues/2");
        assert!(pick(&both, "blktrace", true, |_| None).is_err());
        assert!(pick(&both, "blktrace", true, |_| Some(7)).is_err());
        // Several, unattended: an error that lists them and names --release.
        let err = pick(&both, "blktrace", false, |_| panic!("not asked"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--release") && err.contains("issues/1") && err.contains("issues/2"),
            "{err}"
        );
    }

    fn facts(mr_state: Option<&str>, release: Option<&str>, stream: Option<&str>) -> RetireFacts {
        RetireFacts {
            mr_url: Some("https://gitlab.example/rpms/x/-/merge_requests/1".into()),
            mr_state: mr_state.map(str::to_string),
            release_nvr: release.map(str::to_string),
            stream_nvr: stream.map(str::to_string),
            landed_by: None,
            jira_resolution: None,
            recorded_fix: None,
        }
    }

    #[test]
    fn the_reason_is_read_off_the_facts_and_the_label_says_how_it_got_fixed() {
        // Evidence from stock's history, MR not merged: landed, labelled
        // superseded — upstream got there another way.
        let mut f = facts(
            Some("opened"),
            Some("x-1-13~proposed.el10"),
            Some("x-1-13.el10"),
        );
        f.landed_by = Some("stock commit 99e0f170 (Fix CVE) names CVE-2026-1".into());
        assert_eq!(suggest_reason(&f), Some(Reason::Landed));
        assert!(matches!(reason_check(Reason::Landed, &f), Check::Pass(_)));
        assert_eq!(Reason::Landed.label(f.mr_merged()), "superseded");
        // The SIG's own MR merged: landed proper.
        let mut f = facts(
            Some("merged"),
            Some("x-1-13~proposed.el10"),
            Some("x-1-14.el10"),
        );
        f.landed_by = Some("MR merged".into());
        assert_eq!(Reason::Landed.label(f.mr_merged()), "landed");
        // Stock past the SIG's build and no evidence: the rebase case —
        // no suggestion, and landed fails its check.
        let f = facts(
            Some("closed"),
            Some("x-1-13~proposed.el10"),
            Some("x-1-14.el10"),
        );
        assert!(f.stock_past_without_evidence());
        assert_eq!(suggest_reason(&f), None);
        assert!(matches!(reason_check(Reason::Landed, &f), Check::Fail(_)));
        // Nothing moved, RHEL issue closed as Won't Do: abandoned.
        let mut f = facts(
            Some("opened"),
            Some("x-1-13~proposed.el10"),
            Some("x-1-12.el10"),
        );
        f.jira_resolution = Some("Won't Do".into());
        assert_eq!(suggest_reason(&f), Some(Reason::Abandoned));
        assert!(matches!(
            reason_check(Reason::Abandoned, &f),
            Check::Pass(_)
        ));
        assert_eq!(Reason::Abandoned.label(false), "abandoned");
        // Nothing at all: no suggestion.
        assert_eq!(suggest_reason(&facts(Some("opened"), None, None)), None);
    }

    #[test]
    fn the_reason_prompt_takes_a_word_a_letter_or_the_default() {
        assert_eq!(
            ask_reason(Some(Reason::Landed), |_| Some("\n".into())).unwrap(),
            Reason::Landed
        );
        assert_eq!(
            ask_reason(None, |_| Some("a\n".into())).unwrap(),
            Reason::Abandoned
        );
        assert_eq!(
            ask_reason(None, |_| Some("Fixed\n".into())).unwrap(),
            Reason::Landed
        );
        assert!(ask_reason(None, |_| Some("\n".into())).is_err());
        assert!(ask_reason(Some(Reason::Landed), |_| Some("maybe\n".into())).is_err());
        assert!(ask_reason(None, |_| None).is_err());
    }

    #[test]
    fn the_mr_is_told_in_the_reasons_words() {
        let mut f = facts(
            Some("opened"),
            Some("x-1-13~proposed.el10"),
            Some("x-1-14.el10"),
        );
        let issue = "https://gitlab.example/CentOS/proposed_updates/rpms/x/-/issues/1";
        let abandoned = mr_note_body(Reason::Abandoned, &f, issue);
        assert!(abandoned.contains("withdrawing this change") && abandoned.contains(issue));
        assert!(abandoned.contains("<!-- cpu-sig-tracker: retired abandoned -->"));
        f.landed_by = Some("verified by salimma: the fix is in stock".into());
        let landed = mr_note_body(Reason::Landed, &f, issue);
        assert!(landed.contains("fixed in CentOS Stream now (verified by salimma"));
        assert!(landed.contains("<!-- cpu-sig-tracker: retired superseded -->"));
        assert_eq!(Reason::Abandoned.work_item_status(), "Won't do");
        assert_eq!(Reason::Landed.work_item_status(), "Done");
    }

    fn args(claim: bool, yes: bool) -> RetireArgs {
        RetireArgs {
            issue: "https://gitlab.example/g/p/-/issues/1".to_string(),
            release: None,
            reason: None,
            stock_fix: None,
            yes,
            force: false,
            claim,
            verbose: false,
        }
    }

    #[test]
    fn a_non_interactive_run_does_not_claim_or_even_ask_who_you_are() {
        // -y without --claim declines, per the shared matrix. Asserted
        // here because it returns before any network call or token load:
        // if this regressed, an unattended run would quietly reassign
        // every issue it retired.
        let decided = resolve_issue_claim(&args(false, true)).unwrap();
        assert_eq!(decided, None);
    }

    #[test]
    fn parse_release_from_standard_body() {
        let body = "- **Release**: c10s\n- **Other**: x\n";
        assert_eq!(parse_release_from_body(body).as_deref(), Some("c10s"));
    }

    #[test]
    fn parse_release_returns_none_when_missing() {
        assert_eq!(parse_release_from_body("no release line"), None);
    }

    #[test]
    fn a_hand_filed_issue_names_its_release_with_a_label() {
        let labels: Vec<String> = ["bugfix", "c10s"].map(str::to_string).into();
        assert_eq!(release_from_labels(&labels).as_deref(), Some("c10s"));
        let none: Vec<String> = ["bugfix", "cs", "c1x0s"].map(str::to_string).into();
        assert_eq!(release_from_labels(&none), None);
    }

    #[test]
    fn audit_note_includes_jira_and_build() {
        let note = compose_audit_note(
            Some("RHEL-12345"),
            &Check::Pass("RHEL-12345 — Closed (Done)".to_string()),
            &Check::Pass("no xz build tagged in proposed_updates10s-…".to_string()),
            false,
            Some(("landed", &Check::Pass("MR merged".to_string()))),
        );
        assert!(note.contains("cpu-sig-tracker retire"));
        assert!(note.contains("Reason: landed — MR merged"));
        assert!(note.contains("JIRA RHEL-12345"));
        assert!(note.contains("no xz build"));
        assert!(!note.contains("--force"));
    }

    #[test]
    fn audit_note_marks_force() {
        let note = compose_audit_note(
            None,
            &Check::Skipped("no JIRA key found".to_string()),
            &Check::Fail("still tagged".to_string()),
            true,
            None,
        );
        assert!(note.contains("--force"));
        assert!(note.contains("still tagged"));
    }

    // ---- end-to-end wiremock + fake-binary test ----

    use crate::test_support::{EnvGuard, install_fake_bin};
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{body_partial_json, method, path as wiremock_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RETIRE_ISSUE_BODY: &str = "\
- **MR**: [Fix](https://gitlab.example/foo/bar/-/merge_requests/10) — merged\n\
- **JIRA**: [RHEL-1](https://jira.example/browse/RHEL-1) — Closed (Done)\n\
- **Release**: c10s\n";

    fn koji_empty_list_tagged() -> String {
        "Build  Tag  Built by\n-------  -----  --------\n".to_string()
    }

    #[test]
    #[serial_test::serial]
    fn retire_closes_issue_with_preconditions_passing() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // Fetch the tracking issue.
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 1,
                    "title": "xz retire test",
                    "description": RETIRE_ISSUE_BODY,
                    "state": "opened",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/1",
                    "assignees": [],
                })))
                .mount(&server)
                .await;

            // JIRA lookup — resolved as Done.
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-1",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "Closed" },
                        "resolution": { "name": "Done" },
                        "resolutiondate": "2026-04-20T12:00:00.000+0000"
                    }
                })))
                .mount(&server)
                .await;

            // Audit note POST.
            Mock::given(method("POST"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1/notes",
                ))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
                .expect(1)
                .mount(&server)
                .await;

            // GraphQL endpoint handles both workItemUpdate
            // mutations (status + dates) and the get_work_item_id /
            // resolve_status_id queries that set_work_item_status
            // performs. One catch-all responder covers them all.
            Mock::given(method("POST"))
                .and(wiremock_path("/api/graphql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "project": {
                            "workItems": {
                                "nodes": [{
                                    "id": "gid://gitlab/WorkItem/99",
                                    "widgets": [
                                        { "type": "STATUS", "status": { "name": "Done" } }
                                    ],
                                    "namespace": {
                                        "workItemTypes": {
                                            "nodes": [{
                                                "name": "Issue",
                                                "widgetDefinitions": [{
                                                    "type": "STATUS",
                                                    "allowedStatuses": [{
                                                        "id": "gid://gitlab/status/1",
                                                        "name": "Done"
                                                    }]
                                                }]
                                            }]
                                        }
                                    }
                                }]
                            }
                        },
                        "workItemUpdate": { "errors": [] }
                    }
                })))
                .mount(&server)
                .await;

            // Final PUT that closes the issue.
            Mock::given(method("PUT"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1",
                ))
                .and(body_partial_json(json!({ "state_event": "close" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 1, "title": "t", "state": "closed",
                    "web_url": "https://gitlab.example/…", "assignees": []
                })))
                .expect(1)
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        install_fake_bin(
            dir.path(),
            "koji",
            &[
                // list_tagged_nvrs for the precondition check:
                // no xz build currently tagged → passes.
                (
                    "list-tagged --quiet -- proposed_updates10s-packages-main-release",
                    &koji_empty_list_tagged(),
                ),
            ],
        );
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            // The developer's own config must not leak into a test.
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
            ("PATH", &new_path),
        ]);

        let args = RetireArgs {
            issue: "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/1".to_string(),
            release: None,
            reason: None,
            stock_fix: None,
            yes: true,
            force: false,
            claim: false,
            verbose: false,
        };
        run_inner(&args).expect("retire succeeds");
    }

    /// The same flow with `--claim`: the close must carry the assignee.
    ///
    /// Worth the duplicated mocks. The claim is one field on the last
    /// request of a long sequence, and the failure mode is silent — an
    /// issue that closes without being taken looks exactly like a
    /// successful retire.
    #[test]
    #[serial_test::serial]
    fn retire_with_claim_assigns_the_issue_to_the_caller() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 1,
                    "title": "xz retire test",
                    "description": RETIRE_ISSUE_BODY,
                    "state": "opened",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/1",
                    "assignees": []
                })))
                .mount(&server)
                .await;
            // Who the token belongs to — GitLab assigns by id.
            Mock::given(method("GET"))
                .and(wiremock_path("/api/v4/user"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": 4242, "username": "michel"
                })))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-1",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "Closed" },
                        "resolution": { "name": "Done" },
                        "resolutiondate": "2026-06-01T10:00:00.000+0000"
                    }
                })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1/notes",
                ))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(wiremock_path("/api/graphql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": { "workItemUpdate": { "errors": [] } }
                })))
                .mount(&server)
                .await;
            // The point of the test: closing carries assignee_ids.
            Mock::given(method("PUT"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1",
                ))
                .and(body_partial_json(json!({
                    "state_event": "close",
                    "assignee_ids": [4242]
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 1, "title": "t", "state": "closed",
                    "web_url": "https://gitlab.example/…", "assignees": []
                })))
                .expect(1)
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        install_fake_bin(
            dir.path(),
            "koji",
            &[(
                "list-tagged --quiet -- proposed_updates10s-packages-main-release",
                &koji_empty_list_tagged(),
            )],
        );
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            // The developer's own config must not leak into a test.
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
            ("PATH", &new_path),
        ]);

        // --claim with -y: the flag claims without prompting, which is
        // what makes this testable without a terminal.
        let args = RetireArgs {
            issue: "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/1".to_string(),
            release: None,
            reason: None,
            stock_fix: None,
            yes: true,
            force: false,
            claim: true,
            verbose: false,
        };
        run_inner(&args).expect("retire succeeds");
    }
}
