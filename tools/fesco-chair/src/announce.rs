// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Posting a concluded in-ticket vote's decision, and moving the
//! ticket's labels to match.
//!
//! The chair's half of a decided vote is three edits in a browser:
//! comment the decision, drop `vote-in-progress`, add `pending
//! announcement`. Miss any one and the ticket is still in the vote
//! pool on the next run, so the next agenda asks about it again.
//! Everything needed is already computed by then, so this offers to
//! do it.
//!
//! The decision line is a suggestion, not a verdict to be posted
//! unread. It comes from a tally the tool derived, and a derived
//! tally can be wrong -- a conditional vote is uncounted, a token
//! can be misread, and `--vote`/`--ignore` exist because sometimes
//! only a person can say. So the text is editable at the prompt.

use crate::sources;

/// What the chair chose to do with a decided vote.
pub enum Announced {
    /// Posted and relabelled; the string is what was commented.
    Done(String),
    /// Left alone, to be handled as an ordinary agenda ticket.
    Skipped,
}

/// What an answer to the post-this-decision prompt asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Post,
    Skip,
    Edit,
}

/// Read one answer. `None` is unrecognized, which re-asks rather
/// than falling through to the default: the default here writes to
/// the tracker, so a typo must not be read as consent.
pub fn parse_offer_answer(line: &str) -> Option<Offer> {
    match line.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => Some(Offer::Post),
        "n" | "no" => Some(Offer::Skip),
        "e" | "edit" => Some(Offer::Edit),
        _ => None,
    }
}

/// Read a replacement for `current`, empty keeping it.
fn edit_line(current: &str) -> Result<String, String> {
    use std::io::{BufRead, Write};
    eprint!("  decision [{current}]: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let line = line.trim();
    Ok(if line.is_empty() {
        current.to_string()
    } else {
        line.to_string()
    })
}

/// Show `text`, ask `question` about it, and let it be edited
/// first. `None` is a decline.
///
/// Editable because every caller is offering a line the tool
/// derived -- from a tally, or from a meeting's minutes -- and a
/// derived line is a suggestion. The person is being asked to
/// confirm the text, not merely to consent to it being posted.
fn confirm_text(question: &str, text: &str) -> Result<Option<String>, String> {
    let mut text = text.to_string();
    loop {
        eprintln!("  decision: {text}");
        eprint!("  {question} [Y/n/e to edit]: ");
        use std::io::{BufRead, Write};
        std::io::stderr().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        match parse_offer_answer(&line) {
            Some(Offer::Post) => return Ok(Some(text)),
            Some(Offer::Skip) => return Ok(None),
            Some(Offer::Edit) => text = edit_line(&text)?,
            None => eprintln!("  answer y, n, or e"),
        }
    }
}

/// Offer to announce `decision` on `number`, and do it if accepted.
///
/// `has_vote_label` says whether `vote-in-progress` is there to drop;
/// the label is removed only when it is, so a Fast Track ticket that
/// never carried it is not asked about a label it does not have.
pub fn offer(
    client: &sandogasa_forgejo::Client,
    number: u64,
    decision: &str,
    already: Option<&str>,
    has_vote_label: bool,
    dry_run: bool,
) -> Result<Announced, String> {
    // Shared with the agenda hint, so the prompt and the advice
    // that preceded it cannot describe the edit differently.
    let relabel = crate::votes::relabel(has_vote_label);
    // A decision already in the comments is not posted again. The
    // ticket was decided by hand and only the labels were missed,
    // which is the state that brought it here; commenting over it
    // would announce the same result twice.
    if let Some(existing) = already {
        eprintln!("  already commented: {existing}");
        if !sandogasa_cli::confirm(&format!("{relabel}?"), true).map_err(|e| e.to_string())? {
            return Ok(Announced::Skipped);
        }
        if dry_run {
            eprintln!("  would {relabel}");
        } else {
            relabel_only(client, number, has_vote_label)?;
            eprintln!("  relabelled #{number}");
        }
        return Ok(Announced::Done(existing.to_string()));
    }
    let question = format!("post this decision and {relabel}?");
    let Some(text) = confirm_text(&question, decision)? else {
        return Ok(Announced::Skipped);
    };
    if dry_run {
        eprintln!("  would comment on #{number}: {text}");
        eprintln!("  would {relabel}");
        return Ok(Announced::Done(text));
    }
    write(client, number, &text, has_vote_label)?;
    eprintln!("  commented on #{number} and relabelled");
    Ok(Announced::Done(text))
}

/// Post the comment, then move the labels.
///
/// The comment goes first on purpose: it is the part that records
/// the decision, and a label edit that fails after it leaves a
/// ticket that is merely mislabelled rather than one that is
/// announced with nothing saying what was decided.
fn write(
    client: &sandogasa_forgejo::Client,
    number: u64,
    text: &str,
    has_vote_label: bool,
) -> Result<(), String> {
    let (owner, repo) = (sources::TRACKER_OWNER, sources::TRACKER_REPO);
    client
        .create_comment(owner, repo, number, text)
        .map_err(|e| format!("commenting on #{number}: {e}"))?;
    relabel_only(client, number, has_vote_label)
}

/// Move the labels without touching the comments.
fn relabel_only(
    client: &sandogasa_forgejo::Client,
    number: u64,
    has_vote_label: bool,
) -> Result<(), String> {
    let (owner, repo) = (sources::TRACKER_OWNER, sources::TRACKER_REPO);
    let ids = client
        .label_ids(owner, repo, &[sources::PENDING_LABEL])
        .map_err(|e| format!("resolving `{}`: {e}", sources::PENDING_LABEL))?;
    client
        .add_labels(owner, repo, number, &ids)
        .map_err(|e| format!("tagging #{number} `{}`: {e}", sources::PENDING_LABEL))?;
    if has_vote_label {
        let vote = client
            .label_ids(owner, repo, &[sources::VOTE_LABEL])
            .map_err(|e| format!("resolving `{}`: {e}", sources::VOTE_LABEL))?;
        for id in vote {
            client
                .remove_label(owner, repo, number, id)
                .map_err(|e| format!("untagging #{number} `{}`: {e}", sources::VOTE_LABEL))?;
        }
    }
    Ok(())
}

/// The comment that tells a ticket's followers it is on the agenda.
pub fn meeting_comment(date: chrono::NaiveDate) -> String {
    format!("This issue will be discussed at the next meeting on {date}")
}

/// Offer to put a ticket on the agenda properly: the `meeting`
/// label, and the comment saying when it will be discussed.
///
/// Only a ticket taken from an in-ticket vote reaches this. One that
/// already carried `meeting` was in the meeting pool and never
/// offered, so the label is known to be absent rather than merely
/// unseen — and so is the comment, since the two go on together.
///
/// Declining is not a failure. The reminder printed at the end names
/// whatever was left, so the chair can do it by hand.
pub fn offer_meeting(
    client: &sandogasa_forgejo::Client,
    number: u64,
    date: chrono::NaiveDate,
    dry_run: bool,
) -> Result<bool, String> {
    let question = format!(
        "tag #{number} `{}` and say it will be discussed on {date}?",
        sources::MEETING_LABEL
    );
    if !sandogasa_cli::confirm(&question, true).map_err(|e| e.to_string())? {
        return Ok(false);
    }
    tag_meeting(client, number, date, dry_run)?;
    Ok(true)
}

/// Say when a ticket will be discussed, and label it accordingly.
///
/// Split from the prompt so the writes can be exercised without a
/// terminal: a mistake here is one made against the live tracker.
fn tag_meeting(
    client: &sandogasa_forgejo::Client,
    number: u64,
    date: chrono::NaiveDate,
    dry_run: bool,
) -> Result<(), String> {
    let body = meeting_comment(date);
    if dry_run {
        eprintln!("  would comment on #{number}: {body}");
        eprintln!("  would tag #{number} `{}`", sources::MEETING_LABEL);
        return Ok(());
    }
    let (owner, repo) = (sources::TRACKER_OWNER, sources::TRACKER_REPO);
    client
        .create_comment(owner, repo, number, &body)
        .map_err(|e| format!("commenting on #{number}: {e}"))?;
    let ids = client
        .label_ids(owner, repo, &[sources::MEETING_LABEL])
        .map_err(|e| format!("resolving `{}`: {e}", sources::MEETING_LABEL))?;
    client
        .add_labels(owner, repo, number, &ids)
        .map_err(|e| format!("tagging #{number} `{}`: {e}", sources::MEETING_LABEL))?;
    eprintln!(
        "  commented on #{number} and tagged `{}`",
        sources::MEETING_LABEL
    );
    Ok(())
}

/// `announced` — record that the announcement went out.
#[derive(clap::Args)]
pub struct AnnouncedArgs {
    /// Link to the announcement in the list archive.
    #[arg(value_name = "URL")]
    pub url: String,

    /// Show what would be written without writing it.
    #[arg(long)]
    pub dry_run: bool,

    /// Do not prompt; record every pending ticket.
    #[arg(short = 'y', long = "yes")]
    pub assume_yes: bool,

    /// Consider every pending ticket, without reading the
    /// announcement to see which it actually names.
    #[arg(long)]
    pub all: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

/// The ticket numbers an announcement names.
///
/// Read from the full tracker URLs the agenda template prints under
/// each entry, which survive both the plain-text mail and the
/// archive's HTML rendering of it. A bare `#NNNN` is not matched: in
/// HTML it is as likely to be an anchor as a ticket.
pub fn announced_tickets(body: &str) -> std::collections::BTreeSet<u64> {
    const MARK: &str = "tickets/issues/";
    let mut out = std::collections::BTreeSet::new();
    let mut rest = body;
    while let Some(at) = rest.find(MARK) {
        rest = &rest[at + MARK.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<u64>() {
            out.insert(n);
        }
    }
    out
}

/// Fetch an announcement so its tickets can be read back.
fn fetch_announcement(url: &str) -> Result<String, String> {
    let resp = sources::http_client()
        .get(url)
        .send()
        .map_err(|e| format!("fetching {url}: {e}"))?;
    let resp = sandogasa_cli::http::blocking_ok(resp, "announcement")?;
    resp.text().map_err(|e| format!("reading {url}: {e}"))
}

/// The meetbot minutes an announcement links to.
///
/// A summary mail carries the minutes it summarises, so the meeting
/// does not have to be named on the command line or guessed from the
/// date — and a mail that links no minutes is not a summary, which
/// is the right reason to do nothing.
pub fn minutes_url(body: &str) -> Option<String> {
    const MARK: &str = "https://meetbot.fedoraproject.org/";
    let at = body.find(MARK)?;
    let url: String = body[at..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '<')
        .collect();
    // The plain-text artefact: the HTML one is a page of markup the
    // AGREED lines would have to be dug out of.
    Some(sources::txt_url(&url))
}

/// The status a ticket is closed with, from what was decided.
///
/// `None` when the decision cannot be read, which is a reason to
/// leave the ticket alone rather than to guess: closing it with the
/// wrong status is worse than not closing it.
pub fn status_for(decision: &str) -> Option<&'static str> {
    let d = decision.trim_start().to_ascii_uppercase();
    if d.starts_with("APPROVED") {
        Some(sources::ACCEPTED_LABEL)
    } else if d.starts_with("REJECTED") {
        Some(sources::REJECTED_LABEL)
    } else {
        None
    }
}

/// The comment recording where the announcement went.
pub fn announced_comment(url: &str) -> String {
    format!("Announced: {url}")
}

/// The comment closing a ticket the meeting itself decided.
///
/// One comment rather than two: the decision was never posted to
/// the ticket -- it was said in the meeting -- so the record of
/// what was decided and the link to where it was announced are the
/// same act, and splitting them would leave a ticket whose history
/// reads as two separate events.
pub fn meeting_decision_comment(decision: &str, url: &str) -> String {
    format!("{decision}\n\n{}", announced_comment(url))
}

/// Close out every ticket still tagged `pending announcement`.
///
/// This is the other half of the reminder the agenda prints: once
/// the mail is sent, each announced ticket wants the archive link,
/// the label off, and closing with the status it was decided with.
/// Done by hand it is four edits per ticket and the label is what
/// the next agenda reads, so one missed ticket is announced twice.
pub fn run(args: &AnnouncedArgs) -> std::process::ExitCode {
    match assemble(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn assemble(args: &AnnouncedArgs) -> Result<(), Box<dyn std::error::Error>> {
    let client = sources::forge_client()?;
    let mut tickets = sources::pending_tickets(&client)?;
    // Not an early return on an empty pool: a meeting's own
    // decisions never pass through `pending announcement`, so
    // "nothing is tagged" is not "nothing to do" — which is how a
    // ticket approved at the 2026-10-20 meeting was left open.
    if tickets.is_empty() {
        eprintln!("nothing tagged `{}`", sources::PENDING_LABEL);
    }
    sources::fill_decisions(&client, &mut tickets, args.verbose);
    // Fetched once: it says both which tickets it names and which
    // meeting it summarises.
    let announcement = match fetch_announcement(&args.url) {
        Ok(body) => Some(body),
        Err(e) => {
            eprintln!("warning: {e}");
            None
        }
    };
    // Which tickets this announcement actually names. A decision
    // may be announced in a reply to an earlier announcement, so
    // "everything pending" is not the same set as "everything this
    // link announced", and closing a ticket against a mail that
    // says nothing about it is a falsehood in its own history.
    let named = match (args.all, announcement.as_deref()) {
        (true, _) | (_, None) => None,
        (false, Some(body)) => {
            let found = announced_tickets(body);
            if found.is_empty() {
                eprintln!(
                    "warning: {} names no tracker ticket; considering every \
                     pending one (pass --all to silence this)",
                    args.url
                );
                None
            } else {
                Some(found)
            }
        }
    };
    if let Some(named) = &named {
        for ticket in &tickets {
            if !named.contains(&ticket.number) {
                eprintln!(
                    "{} is pending but not named in this announcement; left alone",
                    ticket.label()
                );
            }
        }
    }
    let interactive = !args.assume_yes && std::io::IsTerminal::is_terminal(&std::io::stdin());

    // Tickets this meeting decided, which never pass through
    // `pending announcement`: that label means "decided, waiting for
    // a mail", and the summary of the meeting is their mail. Read
    // from the minutes the announcement itself links to.
    let decided = match announcement.as_deref().and_then(minutes_url) {
        Some(url) => match sources::http_client().get(&url).send() {
            Ok(resp) => match resp.text() {
                Ok(text) => sources::extract_decisions(&text),
                Err(e) => {
                    eprintln!("warning: reading {url}: {e}");
                    Default::default()
                }
            },
            Err(e) => {
                eprintln!("warning: fetching {url}: {e}");
                Default::default()
            }
        },
        None => Default::default(),
    };
    for (number, found) in &decided {
        if tickets.iter().any(|t| t.number == *number) {
            continue;
        }
        if let Err(e) = offer_meeting_decision(
            &client,
            *number,
            found,
            &args.url,
            interactive,
            args.dry_run,
        ) {
            eprintln!("  not closed: {e}");
        }
    }

    for ticket in &tickets {
        if named.as_ref().is_some_and(|n| !n.contains(&ticket.number)) {
            continue;
        }
        let Some(decision) = ticket.decision.as_deref() else {
            eprintln!(
                "\n{} \u{201c}{}\u{201d}\n  no decision found in its comments, so it is \
                 left open: close it by hand with the status it was decided with",
                ticket.label(),
                ticket.title
            );
            continue;
        };
        let Some(status) = status_for(decision) else {
            eprintln!(
                "\n{} \u{201c}{}\u{201d}\n  decision {decision} names no status, so it is \
                 left open",
                ticket.label(),
                ticket.title
            );
            continue;
        };
        eprintln!(
            "\n{} \u{201c}{}\u{201d}\n  {}\n  {decision} \u{2192} close with `{status}`",
            ticket.label(),
            ticket.title,
            ticket.url
        );
        if interactive && !sandogasa_cli::confirm("record the announcement and close it?", true)? {
            continue;
        }
        if let Err(e) = record(
            &client,
            ticket.number,
            &announced_comment(&args.url),
            &[sources::PENDING_LABEL],
            status,
            args.dry_run,
        ) {
            eprintln!("  not closed: {e}");
        }
    }
    Ok(())
}

/// A comment body as the dry run shows it, indented under the
/// `would comment:` line that introduces it.
///
/// A blank line is left blank rather than indented: the blank
/// separating a decision from its archive link is part of the
/// comment, and four spaces is not a blank line.
fn indented(body: &str) -> String {
    body.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("    {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Comment, untag, tag the status, close — in that order.
///
/// The comment first for the same reason as a decision: it is the
/// record. The labels come off before the close so that a failure
/// part-way leaves the ticket visible to the next run rather than
/// closed and forgotten.
///
/// `drop` is the labels to take off, which differ by route: an
/// announced ticket sheds `pending announcement`, a ticket the
/// meeting itself decided sheds `vote-in-progress` — and only when
/// it has one, since removing a label a ticket never carried is an
/// error the tracker is entitled to raise.
fn record(
    client: &sandogasa_forgejo::Client,
    number: u64,
    body: &str,
    drop: &[&str],
    status: &str,
    dry_run: bool,
) -> Result<(), String> {
    let (owner, repo) = (sources::TRACKER_OWNER, sources::TRACKER_REPO);
    if dry_run {
        eprintln!("  would comment:\n{}", indented(body));
        for label in drop {
            eprintln!("  would untag `{label}`");
        }
        eprintln!("  would tag `{status}` and close");
        return Ok(());
    }
    client
        .create_comment(owner, repo, number, body)
        .map_err(|e| format!("commenting on #{number}: {e}"))?;
    for label in drop {
        let ids = client
            .label_ids(owner, repo, &[label])
            .map_err(|e| format!("resolving `{label}`: {e}"))?;
        for id in ids {
            client
                .remove_label(owner, repo, number, id)
                .map_err(|e| format!("untagging #{number} `{label}`: {e}"))?;
        }
    }
    let ids = client
        .label_ids(owner, repo, &[status])
        .map_err(|e| format!("resolving `{status}`: {e}"))?;
    client
        .add_labels(owner, repo, number, &ids)
        .map_err(|e| format!("tagging #{number} `{status}`: {e}"))?;
    client
        .close_issue(owner, repo, number)
        .map_err(|e| format!("closing #{number}: {e}"))?;
    eprintln!("  recorded and closed #{number} as `{status}`");
    Ok(())
}

/// Close out a ticket the meeting itself decided.
///
/// These never pass through `pending announcement`: that label
/// means "decided in its ticket, waiting for a mail to go out", and
/// a ticket decided at the meeting is announced by the summary of
/// that meeting. So nothing tags them, and before this they stayed
/// open with `vote-in-progress` still on until someone noticed.
///
/// The AGREED line is shown verbatim beside the verdict read from
/// it. The minutes are a transcript of a live meeting -- a chair
/// can agree two things in one line, or write the tally loosely --
/// so the person confirms the text rather than the tool asserting
/// it, and the derived line is only the prepopulated default.
fn offer_meeting_decision(
    client: &sandogasa_forgejo::Client,
    number: u64,
    found: &sources::MeetingDecision,
    url: &str,
    interactive: bool,
    dry_run: bool,
) -> Result<(), String> {
    let (owner, repo) = (sources::TRACKER_OWNER, sources::TRACKER_REPO);
    let issue = client
        .issue(owner, repo, number)
        .map_err(|e| format!("reading #{number}: {e}"))?;
    // Already closed is the common case: most of a meeting's
    // tickets were handled when they were discussed. Saying so
    // beats silence, since a ticket missing from the run's output
    // otherwise looks like one the minutes failed to parse.
    if issue.state != "open" {
        eprintln!(
            "\n#{number} \u{201c}{}\u{201d}\n  already closed",
            issue.title
        );
        return Ok(());
    }
    let Some(status) = status_for(&found.decision) else {
        eprintln!(
            "\n#{number} \u{201c}{}\u{201d}\n  AGREED: {}\n  names no status, so it is left open",
            issue.title, found.agreed
        );
        return Ok(());
    };
    let has_vote_label = issue.labels.iter().any(|l| l.name == sources::VOTE_LABEL);
    eprintln!(
        "\n#{number} \u{201c}{}\u{201d}\n  {}\n  decided at the meeting, never tagged `{}`\n  \
         AGREED: {}",
        issue.title,
        issue.html_url,
        sources::PENDING_LABEL,
        found.agreed
    );
    let question = format!("close it with `{status}`?");
    let text = if interactive {
        match confirm_text(&question, &found.decision)? {
            Some(text) => text,
            None => return Ok(()),
        }
    } else {
        eprintln!("  decision: {}", found.decision);
        found.decision.clone()
    };
    let drop: &[&str] = if has_vote_label {
        &[sources::VOTE_LABEL]
    } else {
        &[]
    };
    record(
        client,
        number,
        &meeting_decision_comment(&text, url),
        drop,
        status,
        dry_run,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tracker that answers the label lookup, so the write paths
    /// can be exercised without one.
    fn tracker() -> mockito::ServerGuard {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/v1/repos/fesco/tickets/labels")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                r#"[{"id": 19, "name": "vote-in-progress"},
                    {"id": 42, "name": "pending announcement"},
                    {"id": 61, "name": "meeting"},
                    {"id": 77, "name": "Status/Accepted"}]"#,
            )
            .expect_at_least(1)
            .create();
        server
    }

    #[test]
    fn announcing_a_decision_comments_then_moves_the_labels() {
        let mut server = tracker();
        let comment = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3691/comments")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"body": "APPROVED (+4, 0, -0)"}),
            ))
            .with_status(201)
            .with_body(r#"{"id": 1, "body": "APPROVED (+4, 0, -0)"}"#)
            .create();
        let add = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3691/labels")
            .match_body(mockito::Matcher::Json(serde_json::json!({"labels": [42]})))
            .with_status(200)
            .with_body(r#"[{"id": 42, "name": "pending announcement"}]"#)
            .create();
        let drop = server
            .mock(
                "DELETE",
                "/api/v1/repos/fesco/tickets/issues/3691/labels/19",
            )
            .with_status(204)
            .create();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        write(&client, 3691, "APPROVED (+4, 0, -0)", true).unwrap();
        comment.assert();
        add.assert();
        drop.assert();
    }

    #[test]
    fn a_ticket_without_the_vote_label_has_none_removed() {
        let mut server = tracker();
        let add = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/labels")
            .with_status(200)
            .with_body(r#"[{"id": 42, "name": "pending announcement"}]"#)
            .create();
        // No DELETE is registered, so any attempt to remove a label
        // fails the test rather than passing unnoticed.
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        relabel_only(&client, 3696, false).unwrap();
        add.assert();
    }

    #[test]
    fn recording_an_announcement_comments_untags_tags_and_closes() {
        let mut server = tracker();
        let url = "https://lists.example/archives/message/ABC/";
        let comment = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3690/comments")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "body": format!("Announced: {url}")
            })))
            .with_status(201)
            .with_body(r#"{"id": 2, "body": "x"}"#)
            .create();
        let untag = server
            .mock(
                "DELETE",
                "/api/v1/repos/fesco/tickets/issues/3690/labels/42",
            )
            .with_status(204)
            .create();
        let status = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3690/labels")
            .match_body(mockito::Matcher::Json(serde_json::json!({"labels": [77]})))
            .with_status(200)
            .with_body(r#"[{"id": 77, "name": "Status/Accepted"}]"#)
            .create();
        let close = server
            .mock("PATCH", "/api/v1/repos/fesco/tickets/issues/3690")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"state": "closed"}),
            ))
            .with_status(200)
            .with_body(r#"{"number": 3690, "title": "t", "state": "closed"}"#)
            .create();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        record(
            &client,
            3690,
            &announced_comment(url),
            &[sources::PENDING_LABEL],
            sources::ACCEPTED_LABEL,
            false,
        )
        .unwrap();
        comment.assert();
        untag.assert();
        status.assert();
        close.assert();
    }

    #[test]
    fn a_dry_run_writes_nothing() {
        // No mock but the label lookup is registered, so any write
        // at all would fail the request and surface as an error.
        let server = tracker();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        record(
            &client,
            3690,
            &announced_comment("https://lists.example/m/"),
            &[sources::PENDING_LABEL],
            sources::ACCEPTED_LABEL,
            true,
        )
        .unwrap();
    }

    #[test]
    fn a_refused_write_is_reported_and_names_the_ticket() {
        let mut server = tracker();
        server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3691/comments")
            .with_status(403)
            .with_body("token has no write scope")
            .create();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        let err = write(&client, 3691, "APPROVED (+4, 0, -0)", true)
            .expect_err("a refused comment is not a success");
        assert!(err.contains("#3691"), "{err}");
        assert!(err.contains("403"), "{err}");
    }

    #[test]
    fn tagging_for_the_meeting_says_when_and_labels_it() {
        let mut server = tracker();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let comment = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/comments")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "body": "This issue will be discussed at the next meeting on 2026-10-20"
            })))
            .with_status(201)
            .with_body(r#"{"id": 3, "body": "x"}"#)
            .create();
        let label = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/labels")
            .match_body(mockito::Matcher::Json(serde_json::json!({"labels": [61]})))
            .with_status(200)
            .with_body(r#"[{"id": 61, "name": "meeting"}]"#)
            .create();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        tag_meeting(&client, 3696, date, false).unwrap();
        comment.assert();
        label.assert();
    }

    #[test]
    fn a_dry_run_of_the_meeting_tag_writes_nothing() {
        let server = tracker();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        tag_meeting(&client, 3696, date, true).unwrap();
    }

    #[test]
    fn the_announcement_is_fetched_and_its_failures_are_reported() {
        let mut server = mockito::Server::new();
        let body = "#3690 Change: Crystal Language\n\
                    https://forge.fedoraproject.org/fesco/tickets/issues/3690\n";
        server
            .mock("GET", "/archives/message/ABC/")
            .with_status(200)
            .with_body(body)
            .create();
        let text = fetch_announcement(&format!("{}/archives/message/ABC/", server.url())).unwrap();
        assert_eq!(
            announced_tickets(&text),
            std::collections::BTreeSet::from([3690])
        );

        // A link that 404s is reported rather than read as an
        // announcement naming nothing, which would widen the run to
        // every pending ticket.
        server
            .mock("GET", "/archives/message/GONE/")
            .with_status(404)
            .with_body("no such message")
            .create();
        let err = fetch_announcement(&format!("{}/archives/message/GONE/", server.url()))
            .expect_err("404 is not an announcement");
        assert!(err.contains("404"), "{err}");
    }

    #[test]
    fn a_refused_meeting_tag_names_the_ticket() {
        let mut server = tracker();
        server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/comments")
            .with_status(403)
            .with_body("nope")
            .create();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        let err = tag_meeting(&client, 3696, date, false).expect_err("403 is not success");
        assert!(err.contains("#3696"), "{err}");
    }

    /// An open ticket the meeting decided, still carrying the vote
    /// label it was never relieved of.
    fn open_issue(server: &mut mockito::ServerGuard) -> mockito::Mock {
        server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues/3696")
            .with_status(200)
            .with_body(
                r#"{"number": 3696, "title": "Incompatible update exception for gedit",
                     "state": "open",
                     "html_url": "https://pagure.io/fesco/tickets/issue/3696",
                     "labels": [{"id": 19, "name": "vote-in-progress"},
                                {"id": 5, "name": "fast track"}]}"#,
            )
            .create()
    }

    fn decided() -> sources::MeetingDecision {
        sources::MeetingDecision {
            decision: "APPROVED (+6, 0, -0)".into(),
            agreed: "Incompatible update exception for gedit and libgedit-* approved (+6, 0, -0)"
                .into(),
        }
    }

    #[test]
    fn a_ticket_the_meeting_decided_is_recorded_untagged_and_closed() {
        let mut server = tracker();
        let read = open_issue(&mut server);
        let url = "https://lists.example/archives/message/ABC/";
        let comment = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/comments")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "body": format!("APPROVED (+6, 0, -0)\n\nAnnounced: {url}")
            })))
            .with_status(201)
            .with_body(r#"{"id": 3, "body": "x"}"#)
            .create();
        // The vote label, not `pending announcement`: it never had
        // one, and that is the whole reason it was still open.
        let untag = server
            .mock(
                "DELETE",
                "/api/v1/repos/fesco/tickets/issues/3696/labels/19",
            )
            .with_status(204)
            .create();
        let status = server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/labels")
            .match_body(mockito::Matcher::Json(serde_json::json!({"labels": [77]})))
            .with_status(200)
            .with_body(r#"[{"id": 77, "name": "Status/Accepted"}]"#)
            .create();
        let close = server
            .mock("PATCH", "/api/v1/repos/fesco/tickets/issues/3696")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"state": "closed"}),
            ))
            .with_status(200)
            .with_body(r#"{"number": 3696, "title": "t", "state": "closed"}"#)
            .create();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        offer_meeting_decision(&client, 3696, &decided(), url, false, false).unwrap();
        read.assert();
        comment.assert();
        untag.assert();
        status.assert();
        close.assert();
    }

    #[test]
    fn a_ticket_closed_at_the_meeting_is_left_alone() {
        let mut server = tracker();
        // No write mock: the server rejects anything unmatched, so
        // a second pass over an already-handled meeting that wrote
        // would fail here rather than duplicate a comment.
        server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues/3696")
            .with_status(200)
            .with_body(r#"{"number": 3696, "title": "t", "state": "closed"}"#)
            .create();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        offer_meeting_decision(&client, 3696, &decided(), "https://x/", false, false).unwrap();
    }

    #[test]
    fn a_meeting_decision_naming_no_status_writes_nothing() {
        let mut server = tracker();
        open_issue(&mut server);
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        let vague = sources::MeetingDecision {
            decision: "DEFERRED (+0, 0, -0)".into(),
            agreed: "deferred to next week (+0, 0, -0)".into(),
        };
        offer_meeting_decision(&client, 3696, &vague, "https://x/", false, false).unwrap();
    }

    #[test]
    fn a_ticket_without_the_vote_label_has_none_dropped() {
        let mut server = tracker();
        server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues/3696")
            .with_status(200)
            .with_body(r#"{"number": 3696, "title": "t", "state": "open", "labels": []}"#)
            .create();
        server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/comments")
            .with_status(201)
            .with_body(r#"{"id": 3, "body": "x"}"#)
            .create();
        server
            .mock("POST", "/api/v1/repos/fesco/tickets/issues/3696/labels")
            .with_status(200)
            .with_body(r#"[{"id": 77, "name": "Status/Accepted"}]"#)
            .create();
        server
            .mock("PATCH", "/api/v1/repos/fesco/tickets/issues/3696")
            .with_status(200)
            .with_body(r#"{"number": 3696, "title": "t", "state": "closed"}"#)
            .create();
        let client = sandogasa_forgejo::Client::new(&server.url(), "tok").unwrap();
        // A DELETE would go unmatched and fail; that it does not is
        // the assertion.
        offer_meeting_decision(&client, 3696, &decided(), "https://x/", false, false).unwrap();
    }

    #[test]
    fn the_minutes_come_from_the_summary_as_plain_text() {
        let body = "Minutes: https://meetbot.fedoraproject.org/fedora-meeting/\
                    2026-10-20/fesco.2026-10-20-19.00.html\nLog: elsewhere\n";
        assert_eq!(
            minutes_url(body).as_deref(),
            Some(
                "https://meetbot.fedoraproject.org/fedora-meeting/2026-10-20/\
                 fesco.2026-10-20-19.00.txt"
            )
        );
    }

    #[test]
    fn a_summary_linking_no_minutes_names_none() {
        assert_eq!(minutes_url("FESCo met on Monday. Nothing to see.\n"), None);
    }

    #[test]
    fn a_minutes_link_in_html_stops_at_the_quote() {
        let body = r#"<a href="https://meetbot.fedoraproject.org/x/fesco.html">minutes</a>"#;
        assert_eq!(
            minutes_url(body).as_deref(),
            Some("https://meetbot.fedoraproject.org/x/fesco.txt")
        );
    }

    #[test]
    fn a_shown_comment_is_indented_but_its_blank_line_is_not_padded() {
        let body = meeting_decision_comment("APPROVED (+6, 0, -0)", "https://lists.example/m/");
        let shown = indented(&body);
        assert!(shown.starts_with("    APPROVED (+6, 0, -0)"), "{shown:?}");
        assert!(shown.contains("\n\n    Announced:"), "{shown:?}");
        sources::assert_no_trailing_whitespace("dry-run comment", &shown);
    }

    #[test]
    fn the_comments_posted_never_end_a_line_in_whitespace() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        sources::assert_no_trailing_whitespace("meeting comment", &meeting_comment(date));
        sources::assert_no_trailing_whitespace(
            "announced comment",
            &announced_comment("https://lists.example/m/"),
        );
        sources::assert_no_trailing_whitespace(
            "meeting decision comment",
            &meeting_decision_comment("APPROVED (+6, 0, -0)", "https://lists.example/m/"),
        );
    }

    #[test]
    fn the_meeting_comment_records_the_decision_and_the_link() {
        let got = meeting_decision_comment("APPROVED (+6, 0, -0)", "https://lists.example/m/");
        assert!(got.starts_with("APPROVED (+6, 0, -0)"), "{got}");
        assert!(
            got.ends_with("Announced: https://lists.example/m/"),
            "{got}"
        );
    }

    #[test]
    fn an_answer_to_the_post_prompt_is_read_strictly() {
        assert_eq!(parse_offer_answer(""), Some(Offer::Post));
        assert_eq!(parse_offer_answer(" Y \n"), Some(Offer::Post));
        assert_eq!(parse_offer_answer("yes"), Some(Offer::Post));
        assert_eq!(parse_offer_answer("n"), Some(Offer::Skip));
        assert_eq!(parse_offer_answer("NO"), Some(Offer::Skip));
        assert_eq!(parse_offer_answer("e"), Some(Offer::Edit));
        assert_eq!(parse_offer_answer("edit"), Some(Offer::Edit));
        // Enter posts, so a typo must re-ask rather than be read as
        // consent to write to the tracker.
        assert_eq!(parse_offer_answer("yep"), None);
        assert_eq!(parse_offer_answer("q"), None);
    }

    #[test]
    fn the_status_a_ticket_closes_with_comes_from_its_decision() {
        assert_eq!(
            status_for("APPROVED (+5, 0, -0)"),
            Some(sources::ACCEPTED_LABEL)
        );
        assert_eq!(
            status_for("REJECTED (+0, 0, -7)"),
            Some(sources::REJECTED_LABEL)
        );
        // extract_decision normalises to the verdict, but a hand
        // written line may still be indented.
        assert_eq!(
            status_for("  approved (+3, 0, 0)"),
            Some(sources::ACCEPTED_LABEL)
        );
        // Anything else leaves the ticket open rather than guessing:
        // the wrong status is worse than no close.
        assert_eq!(status_for("DEFERRED to next week"), None);
        assert_eq!(status_for(""), None);
    }

    #[test]
    fn an_announcement_names_its_tickets_by_url() {
        // The agenda template prints the full tracker URL under each
        // entry, which is what survives the archive's HTML.
        let body = "= Discussed and Voted in the Ticket =\n\n\
                    #3690 Change: Crystal Language\n\
                    https://forge.fedoraproject.org/fesco/tickets/issues/3690\n\
                    APPROVED (+5, 0, -0)\n";
        assert_eq!(
            announced_tickets(body),
            std::collections::BTreeSet::from([3690])
        );

        // The label-filter link in the header is not a ticket.
        let header = "Links to all issues to be discussed can be found at:\n\
                      https://forge.fedoraproject.org/fesco/tickets/issues?labels=6114\n";
        assert!(announced_tickets(header).is_empty());

        // An HTML rendering with the URL inside an anchor still
        // yields it, which is how the archive serves the mail.
        let html = r#"<a href="https://forge.fedoraproject.org/fesco/tickets/issues/3691">x</a>"#;
        assert_eq!(
            announced_tickets(html),
            std::collections::BTreeSet::from([3691])
        );

        // A reply announcing one decision names only that one, which
        // is the whole point of reading it.
        let reply = "Announcing the approval of\n\
                     https://forge.fedoraproject.org/fesco/tickets/issues/3691\n";
        assert_eq!(
            announced_tickets(reply),
            std::collections::BTreeSet::from([3691])
        );
        assert!(announced_tickets("no tickets here").is_empty());
    }

    #[test]
    fn the_meeting_comment_names_the_meeting_date() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        assert_eq!(
            meeting_comment(d),
            "This issue will be discussed at the next meeting on 2026-10-20"
        );
    }

    #[test]
    fn the_recorded_comment_carries_the_archive_link() {
        let url = "https://lists.fedoraproject.org/archives/list/devel@lists.fedoraproject.org/thread/ABC/";
        assert_eq!(announced_comment(url), format!("Announced: {url}"));
    }
}
