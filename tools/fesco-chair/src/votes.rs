// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `votes` subcommand — where every open in-ticket vote stands under
//! the FESCo ticket-vote policy
//! (<https://docs.fedoraproject.org/en-US/fesco/#ticket-votes>).
//!
//! Tickets labeled `vote-in-progress` or `fast track` are scanned;
//! the latest `+1` / `0` / `-1` from each currently serving FESCo
//! member (the `fesco` FAS group) is tallied, the clock starts at the
//! label that opened the vote, and the policy's rules give a verdict:
//! approved, rejected, needs a meeting (any `-1`), or waiting — with
//! the date the ticket becomes decidable, so the chair can see before
//! composing the agenda whether to let a vote run or bring it to the
//! meeting. Fast Track tickets also report whether the policy's
//! 48-hour reminder has gone out.
//!
//! The per-member lists are for the chair's eyes: a quiet reminder to
//! those who have not voted, and a running tally when votes change
//! during the meeting — not for naming anyone in the announcement.

use std::collections::BTreeMap;
use std::process::ExitCode;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sandogasa_forgejo::TimelineEvent;

use crate::sources::{self, FAST_TRACK_LABEL, TRACKER_OWNER, TRACKER_REPO, Ticket, VOTE_LABEL};

/// The FAS group whose members are the eligible voters.
pub const FESCO_GROUP: &str = "fesco";
/// The title marker the policy requires on Fast Track tickets.
pub const FAST_TRACK_TITLE: &str = "[FastTrack]";

#[derive(clap::Args)]
pub struct VotesArgs {
    /// Meeting date the hints refer to (default: the coming Tuesday).
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub date: Option<NaiveDate>,

    /// Count these FAS users as voters instead of the fesco group
    /// (repeat/CSV).
    #[arg(long = "member", value_name = "FAS", value_delimiter = ',')]
    pub members: Vec<String>,

    /// Group members who do not vote, e.g. the FPL (repeat/CSV).
    #[arg(long = "non-voting", value_name = "FAS", value_delimiter = ',')]
    pub non_voting: Vec<String>,

    /// Count a member's vote on one ticket as given, e.g.
    /// 3685:salimma=0, when a comment was misread (repeat/CSV).
    #[arg(long = "vote", value_name = "N:FAS=V", value_delimiter = ',', value_parser = parse_override)]
    pub votes: Vec<(u64, String, Vote)>,

    /// Treat a member as not having voted on one ticket, e.g.
    /// 3685:salimma (repeat/CSV).
    #[arg(long = "ignore", value_name = "N:FAS", value_delimiter = ',', value_parser = parse_scoped)]
    pub ignore: Vec<(u64, String)>,

    /// Machine-readable JSON output.
    #[arg(long)]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

/// An official vote as the policy defines it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Vote {
    Plus,
    Zero,
    Minus,
}

/// Each member's latest vote, split by value; `silent` members have
/// not voted.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Tally {
    pub plus: Vec<String>,
    pub zero: Vec<String>,
    pub minus: Vec<String>,
    pub silent: Vec<String>,
}

impl Tally {
    /// The counts in the `(+X, Y, -Z)` form the decision comments use.
    pub fn counts(&self) -> String {
        format!(
            "(+{}, {}, -{})",
            self.plus.len(),
            self.zero.len(),
            self.minus.len()
        )
    }
}

/// What the policy says about a ticket right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Approved,
    Rejected,
    /// A `-1` stands: the ticket goes on the meeting agenda.
    Meeting,
    Waiting,
}

/// The verdict with its reasoning and, while waiting, the moment the
/// next rule fires.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct Verdict {
    pub outcome: Outcome,
    pub detail: String,
    /// How many more +1 the ticket needs to conclude at `decidable_at`
    /// without further votes; zero once it is decided or will be.
    pub votes_needed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decidable_at: Option<DateTime<Utc>>,
}

/// Whether the Fast Track 48-hour reminder went out.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Reminder {
    Sent { at: DateTime<Utc>, by: String },
    Overdue { since: DateTime<Utc> },
    Due { at: DateTime<Utc> },
}

/// One scanned ticket.
#[derive(Debug, serde::Serialize)]
pub struct VoteReport {
    #[serde(flatten)]
    pub ticket: Ticket,
    pub fast_track: bool,
    /// When the clock started and which event set it.
    pub start: DateTime<Utc>,
    pub start_basis: &'static str,
    pub votes: Tally,
    /// [`Tally::counts`], for the announcement.
    pub tally: String,
    #[serde(flatten)]
    pub verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reminder: Option<Reminder>,
}

#[derive(serde::Serialize)]
struct VotesJson<'a> {
    date: String,
    members: &'a [String],
    tickets: &'a [VoteReport],
}

/// The vote in one comment, if any: the last `+1`, `-1` or explicit
/// zero (`0` signed one way or another) in the text, or a bare `0`
/// when nothing signed appears. Quoted lines and tally lines
/// (`APPROVED (+3, 0, 0)`) are skipped; markdown and punctuation
/// around the token are ignored, so `**-1**`, `(+1)` and `+1.` all
/// count.
/// Tokens are taken at face value: "I'll go -1 if we hear nothing"
/// counts as a `-1`, since telling a conditional from a vote would
/// need to read the sentence — the report names the voter so the
/// chair can check the ticket.
pub fn parse_vote(body: &str) -> Option<Vote> {
    let mut explicit = None;
    let mut bare_zero = false;
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with('>') || t.contains("(+") {
            continue;
        }
        for word in t.split_whitespace() {
            let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && !"+-±".contains(c));
            match w {
                "+1" => explicit = Some(Vote::Plus),
                "-1" => explicit = Some(Vote::Minus),
                "+0" | "-0" | "±0" | "+-0" | "+/-0" => explicit = Some(Vote::Zero),
                "0" => bare_zero = true,
                _ => {}
            }
        }
    }
    explicit.or(bare_zero.then_some(Vote::Zero))
}

/// An `--ignore N:FAS` value.
fn parse_scoped(s: &str) -> Result<(u64, String), String> {
    let (n, who) = s
        .split_once(':')
        .ok_or_else(|| format!("expected TICKET:FAS, got `{s}`"))?;
    let n = n
        .trim_start_matches('#')
        .parse()
        .map_err(|_| format!("`{n}` is not a ticket number"))?;
    Ok((n, who.to_string()))
}

/// A `--vote N:FAS=V` value.
fn parse_override(s: &str) -> Result<(u64, String, Vote), String> {
    let (scoped, vote) = s
        .split_once('=')
        .ok_or_else(|| format!("expected TICKET:FAS=+1|0|-1, got `{s}`"))?;
    let (n, who) = parse_scoped(scoped)?;
    let vote = parse_vote(vote).ok_or_else(|| format!("`{vote}` is not a vote (+1, 0, -1)"))?;
    Ok((n, who, vote))
}

/// The comments in `events`, oldest first, as `(login, body, at)`.
fn comments(events: &[TimelineEvent]) -> impl Iterator<Item = (&str, &str, Option<DateTime<Utc>>)> {
    events.iter().filter(|e| e.kind == "comment").map(|e| {
        (
            e.user.as_ref().map_or("", |u| u.login.as_str()),
            e.body.as_str(),
            e.created_at.as_deref().and_then(parse_time),
        )
    })
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Tally the latest vote of each member across `comments` (oldest
/// first, so a later comment supersedes an earlier one). Votes from
/// anyone else are ignored. `overrides` win over the comments: a vote
/// to count instead, or `None` to count the member as not voting.
pub fn tally<'a>(
    comments: impl Iterator<Item = (&'a str, &'a str)>,
    members: &[String],
    overrides: &BTreeMap<String, Option<Vote>>,
) -> Tally {
    let mut latest: BTreeMap<&str, Vote> = BTreeMap::new();
    for (login, body) in comments {
        if members.iter().any(|m| m == login)
            && let Some(vote) = parse_vote(body)
        {
            latest.insert(login, vote);
        }
    }
    for (who, vote) in overrides {
        match vote {
            Some(v) => latest.insert(who, *v),
            None => latest.remove(who.as_str()),
        };
    }
    let mut out = Tally::default();
    for m in members {
        match latest.get(m.as_str()) {
            Some(Vote::Plus) => out.plus.push(m.clone()),
            Some(Vote::Zero) => out.zero.push(m.clone()),
            Some(Vote::Minus) => out.minus.push(m.clone()),
            None => out.silent.push(m.clone()),
        }
    }
    out
}

/// When `label` was last added, from the timeline.
pub fn label_added_at(events: &[TimelineEvent], label: &str) -> Option<DateTime<Utc>> {
    events
        .iter()
        .rev()
        .find(|e| e.label_added() == Some(label))
        .and_then(|e| e.created_at.as_deref())
        .and_then(parse_time)
}

/// When the vote's clock started: the `vote-in-progress` label, else
/// the `fast track` label, else the ticket's creation (a Change
/// ticket is a proposal on creation), with the basis named for the
/// report.
pub fn vote_start(
    events: &[TimelineEvent],
    created: DateTime<Utc>,
) -> (DateTime<Utc>, &'static str) {
    if let Some(t) = label_added_at(events, VOTE_LABEL) {
        (t, "vote-in-progress label")
    } else if let Some(t) = label_added_at(events, FAST_TRACK_LABEL) {
        (t, "fast track label")
    } else {
        (created, "ticket creation")
    }
}

/// Apply the ticket-vote rules to a tally at `now`. Rule order
/// follows the policy: a `-1` sends the ticket to the meeting (and
/// drops it from Fast Track), `+7` with no `-1` approves a Fast Track
/// ticket at once, after one week `+3` approves and seven `-1` reject,
/// a second week lowers the bar to a single `+1`, and no votes at all
/// after two weeks rejects.
pub fn classify(
    tally: &Tally,
    fast_track: bool,
    start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Verdict {
    let (plus, minus) = (tally.plus.len(), tally.minus.len());
    let week = start + Duration::days(7);
    let two_weeks = start + Duration::days(14);
    let day = |t: DateTime<Utc>| t.format("%Y-%m-%d");
    let verdict = |outcome, detail: String, decidable_at, votes_needed| Verdict {
        outcome,
        detail,
        votes_needed,
        decidable_at,
    };
    if minus >= 7 && plus == 0 && now >= week {
        return verdict(
            Outcome::Rejected,
            format!("{minus} votes of -1 and no +1 after a week"),
            None,
            0,
        );
    }
    if minus > 0 {
        let ft = if fast_track {
            " and drops it from Fast Track"
        } else {
            ""
        };
        return verdict(
            Outcome::Meeting,
            format!(
                "-1 from {}: a -1 sends the ticket to the meeting agenda{ft}",
                tally.minus.join(", ")
            ),
            None,
            0,
        );
    }
    if fast_track && plus >= 7 {
        return verdict(
            Outcome::Approved,
            format!("Fast Track: +{plus} with no -1"),
            None,
            0,
        );
    }
    if now >= two_weeks {
        return if plus >= 1 {
            verdict(
                Outcome::Approved,
                format!("after two weeks: +{plus} with no -1"),
                None,
                0,
            )
        } else {
            verdict(
                Outcome::Rejected,
                "no votes in two weeks: status quo stands".to_string(),
                None,
                0,
            )
        };
    }
    if now >= week {
        return if plus >= 3 {
            verdict(
                Outcome::Approved,
                format!("after a week: +{plus} with no -1"),
                None,
                0,
            )
        } else {
            verdict(
                Outcome::Waiting,
                format!(
                    "+{plus} after a week; second week granted, approved on {} with at least one +1 and no -1",
                    day(two_weeks)
                ),
                Some(two_weeks),
                1usize.saturating_sub(plus),
            )
        };
    }
    // Fast Track only shortens the wait; the week-end rule still
    // decides a ticket that falls short of +7, so that is the count
    // that says whether it can conclude as it stands.
    let needed = 3usize.saturating_sub(plus);
    let mut detail = if plus >= 3 {
        format!("+{plus}: approved on {} unless a -1 arrives", day(week))
    } else {
        format!(
            "+{plus}: needs {} more +1 by {} (else a second week to {})",
            3 - plus,
            day(week),
            day(two_weeks)
        )
    };
    if fast_track {
        detail = format!(
            "{} more +1 approves it now under Fast Track; {detail}",
            7 - plus
        );
    }
    verdict(Outcome::Waiting, detail, Some(week), needed)
}

/// Whether the Fast Track 48-hour reminder went out: a member's
/// comment mentioning "reminder" after the vote opened counts as sent.
pub fn reminder(
    events: &[TimelineEvent],
    members: &[String],
    start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Reminder {
    let sent = comments(events).find_map(|(login, body, at)| {
        let at = at?;
        (at > start
            && members.iter().any(|m| m == login)
            && body.to_ascii_lowercase().contains("reminder"))
        .then(|| (at, login.to_string()))
    });
    let due = start + Duration::hours(48);
    match sent {
        Some((at, by)) => Reminder::Sent { at, by },
        None if now >= due => Reminder::Overdue { since: due },
        None => Reminder::Due { at: due },
    }
}

/// The eligible voters: `--member` when given, else the `fesco` FAS
/// group via FASJSON, which needs a Kerberos ticket (offered when
/// missing), less `--non-voting`. Sorted so every list in the report
/// is stable.
fn members(args: &VotesArgs) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut members = if args.members.is_empty() {
        ensure_kerberos_ticket()?;
        sandogasa_fasjson::FasjsonClient::new()
            .group_members(FESCO_GROUP, None)?
            .into_iter()
            .map(|u| u.username)
            .collect()
    } else {
        args.members.clone()
    };
    members.retain(|m| !args.non_voting.contains(m));
    members.sort();
    members.dedup();
    Ok(members)
}

fn ensure_kerberos_ticket() -> Result<(), Box<dyn std::error::Error>> {
    use sandogasa_fasjson::kerberos::{self, TicketStatus};
    let status = kerberos::ticket_status();
    if status == TicketStatus::Valid
        || (status == TicketStatus::ExpiredRenewable && kerberos::renew_ticket().is_ok())
    {
        return Ok(());
    }
    let remedy = format!(
        "no Kerberos ticket: FASJSON lists the {FESCO_GROUP} group's members; run `kinit <you>@FEDORAPROJECT.ORG` or pass --member"
    );
    let upn = kerberos::read_fedora_upn().ok_or_else(|| remedy.clone())?;
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(remedy.into());
    }
    let principal = format!("{upn}@FEDORAPROJECT.ORG");
    eprintln!("no Kerberos ticket; running kinit {principal}");
    kerberos::acquire_ticket(&principal).map_err(|e| format!("{e}; {remedy}").into())
}

/// Scan the tracker and assemble the report.
fn assemble(
    args: &VotesArgs,
    members: &[String],
    now: DateTime<Utc>,
) -> Result<Vec<VoteReport>, Box<dyn std::error::Error>> {
    let client = sources::forge_client()?;
    let mut overrides: BTreeMap<u64, BTreeMap<String, Option<Vote>>> = BTreeMap::new();
    for (n, who) in &args.ignore {
        overrides.entry(*n).or_default().insert(who.clone(), None);
    }
    for (n, who, v) in &args.votes {
        overrides
            .entry(*n)
            .or_default()
            .insert(who.clone(), Some(*v));
    }
    let mut issues = Vec::new();
    for label in [FAST_TRACK_LABEL, VOTE_LABEL] {
        if args.verbose {
            eprintln!("[votes] fetching open `{label}` tickets");
        }
        issues.extend(client.repo_issues(TRACKER_OWNER, TRACKER_REPO, "open", &[label])?);
    }
    issues.sort_by_key(|i| i.number);
    issues.dedup_by_key(|i| i.number);
    let mut out = Vec::new();
    for issue in issues {
        if args.verbose {
            eprintln!("[votes] reading #{}", issue.number);
        }
        let events = client.issue_timeline(TRACKER_OWNER, TRACKER_REPO, issue.number)?;
        let created = issue
            .created_at
            .as_deref()
            .and_then(parse_time)
            .unwrap_or(now);
        let fast_track = issue.title.contains(FAST_TRACK_TITLE)
            || label_added_at(&events, FAST_TRACK_LABEL).is_some();
        let (start, start_basis) = vote_start(&events, created);
        let votes = tally(
            comments(&events).map(|(l, b, _)| (l, b)),
            members,
            overrides.get(&issue.number).unwrap_or(&BTreeMap::new()),
        );
        let verdict = classify(&votes, fast_track, start, now);
        let tally = votes.counts();
        let reminder = (fast_track && verdict.outcome == Outcome::Waiting).then(|| {
            reminder(
                &events,
                members,
                label_added_at(&events, FAST_TRACK_LABEL).unwrap_or(start),
                now,
            )
        });
        out.push(VoteReport {
            ticket: issue.into(),
            fast_track,
            start,
            start_basis,
            votes,
            tally,
            verdict,
            reminder,
        });
    }
    Ok(out)
}

/// The human-readable report for one ticket, with the chair's hint
/// when the vote cannot conclude before `meeting`.
pub fn render(report: &VoteReport, meeting: NaiveDate) -> String {
    let list = |v: &[String]| {
        if v.is_empty() {
            "-".to_string()
        } else {
            v.join(", ")
        }
    };
    let kind = if report.fast_track {
        "Fast Track"
    } else {
        "ticket vote"
    };
    let mut lines = vec![
        sources::entry(&report.ticket),
        format!(
            "  {kind} since {} ({})",
            report.start.format("%Y-%m-%d %H:%M UTC"),
            report.start_basis
        ),
        format!("  tally {}", report.tally),
        format!("    +1: {}", list(&report.votes.plus)),
        format!("     0: {}", list(&report.votes.zero)),
        format!("    -1: {}", list(&report.votes.minus)),
        format!("    no vote: {}", list(&report.votes.silent)),
        format!(
            "  {}: {}",
            format!("{:?}", report.verdict.outcome).to_lowercase(),
            report.verdict.detail
        ),
    ];
    if let Some(at) = report.verdict.decidable_at {
        lines.push(if at.date_naive() > meeting {
            format!(
                "  not decidable in ticket before the {meeting} meeting: let it run, or a procedural -1 puts it on the agenda"
            )
        } else {
            format!("  decidable in ticket on {}, before the {meeting} meeting", at.format("%Y-%m-%d"))
        });
    }
    if report.verdict.votes_needed > 0 {
        lines.push(format!(
            "  NEEDS VOTES: {} more +1; not voted: {}",
            report.verdict.votes_needed,
            list(&report.votes.silent)
        ));
    }
    match &report.reminder {
        Some(Reminder::Sent { at, by }) => lines.push(format!(
            "  48h reminder: sent {} by {by}",
            at.format("%Y-%m-%d")
        )),
        Some(Reminder::Overdue { since }) => lines.push(format!(
            "  48h reminder: OVERDUE since {} — a FESCo member should send one",
            since.format("%Y-%m-%d %H:%M UTC")
        )),
        Some(Reminder::Due { at }) => lines.push(format!(
            "  48h reminder: due {}",
            at.format("%Y-%m-%d %H:%M UTC")
        )),
        None => {}
    }
    lines.join("\n")
}

pub fn run(args: &VotesArgs) -> ExitCode {
    let now = Utc::now();
    let meeting = args
        .date
        .unwrap_or_else(|| sources::next_tuesday(now.date_naive()));
    let result = members(args).and_then(|members| {
        if args.verbose {
            eprintln!("[votes] voters: {}", members.join(", "));
        }
        assemble(args, &members, now).map(|reports| (members, reports))
    });
    let (members, reports) = match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.json {
        let out = VotesJson {
            date: meeting.to_string(),
            members: &members,
            tickets: &reports,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    } else if reports.is_empty() {
        println!("no open ticket votes ({VOTE_LABEL} / {FAST_TRACK_LABEL})");
    } else {
        let text: Vec<String> = reports.iter().map(|r| render(r, meeting)).collect();
        println!("{}", text.join("\n\n"));
        let short: Vec<String> = reports
            .iter()
            .filter(|r| r.verdict.votes_needed > 0)
            .map(|r| r.ticket.label())
            .collect();
        if !short.is_empty() {
            eprintln!(
                "\nshort of votes, cannot conclude as they stand: {}",
                short.join(", ")
            );
        }
        eprintln!(
            "\nreminder: the per-member lists are for you — a quiet nudge to those \
             who have not voted, and a running tally if votes change during the \
             meeting — not for the announcement"
        );
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        parse_time(s).unwrap()
    }

    fn comment(login: &str, body: &str, when: &str) -> TimelineEvent {
        TimelineEvent {
            kind: "comment".into(),
            body: body.into(),
            user: Some(sandogasa_forgejo::UserRef {
                login: login.into(),
            }),
            created_at: Some(when.into()),
            label: None,
        }
    }

    fn label(name: &str, added: bool, when: &str) -> TimelineEvent {
        TimelineEvent {
            kind: "label".into(),
            body: if added { "1" } else { "" }.into(),
            user: None,
            created_at: Some(when.into()),
            label: Some(sandogasa_forgejo::LabelRef { name: name.into() }),
        }
    }

    fn members(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_vote_reads_the_policy_tokens_through_markdown() {
        assert_eq!(parse_vote("**-1**"), Some(Vote::Minus));
        assert_eq!(parse_vote("I'm a weak +1."), Some(Vote::Plus));
        assert_eq!(parse_vote("+0 for now. Some questions:"), Some(Vote::Zero));
        assert_eq!(parse_vote("I'm 0 on this"), Some(Vote::Zero));
        assert_eq!(parse_vote("Sounds fine to me"), None);
    }

    #[test]
    fn parse_vote_takes_the_last_explicit_vote_and_ignores_quotes_and_tallies() {
        // decathorpe on #3685: the -1 is the one being withdrawn.
        assert_eq!(
            parse_vote("change my -1 vote to a ±0 when viewing this as an exception"),
            Some(Vote::Zero)
        );
        assert_eq!(
            parse_vote("> I am -1\n\nThanks, +1 from me"),
            Some(Vote::Plus)
        );
        assert_eq!(parse_vote("After a week: APPROVED (+3, 0, 0)"), None);
        // A bare 0 does not override a signed vote in the same comment.
        assert_eq!(parse_vote("+1, this breaks 0 packages"), Some(Vote::Plus));
    }

    #[test]
    fn tally_keeps_each_members_latest_vote_and_ignores_outsiders() {
        let m = members(&["decathorpe", "kevin", "zbyszek"]);
        let t = tally(
            [
                ("decathorpe", "-1"),
                ("adamwill", "+1 with my QA hat on"),
                ("kevin", "I'm a weak +1"),
                ("decathorpe", "changing my -1 to a ±0"),
            ]
            .into_iter(),
            &m,
            &BTreeMap::new(),
        );
        assert_eq!(
            t,
            Tally {
                plus: members(&["kevin"]),
                zero: members(&["decathorpe"]),
                minus: vec![],
                silent: members(&["zbyszek"]),
            }
        );
        assert_eq!(t.counts(), "(+1, 1, -0)");
    }

    #[test]
    fn overrides_replace_or_drop_a_misread_vote() {
        let m = members(&["salimma", "zbyszek"]);
        let comments = [
            ("salimma", "I'll do a procedural -1 if we don't hear back"),
            ("zbyszek", "+1"),
        ];
        let overrides = BTreeMap::from([("salimma".to_string(), None)]);
        assert_eq!(
            tally(comments.into_iter(), &m, &overrides).silent,
            members(&["salimma"])
        );
        let overrides = BTreeMap::from([("zbyszek".to_string(), Some(Vote::Zero))]);
        let t = tally(comments.into_iter(), &m, &overrides);
        assert_eq!(
            (t.zero, t.minus),
            (members(&["zbyszek"]), members(&["salimma"]))
        );
        assert_eq!(
            parse_override("3685:salimma=0"),
            Ok((3685, "salimma".into(), Vote::Zero))
        );
        assert_eq!(
            parse_override("#3685:kevin=+1"),
            Ok((3685, "kevin".into(), Vote::Plus))
        );
        assert_eq!(parse_scoped("3685:kevin"), Ok((3685, "kevin".into())));
        assert!(parse_scoped("kevin").is_err());
        assert!(parse_scoped("x:kevin").is_err());
        assert!(parse_override("3685:kevin").is_err());
        assert!(parse_override("3685:kevin=maybe").is_err());
    }

    #[test]
    fn vote_start_prefers_the_vote_label_then_fast_track_then_creation() {
        let created = at("2026-09-15T14:39:04Z");
        let ft = vec![
            label("meeting", true, "2026-09-15T14:39:10Z"),
            label("fast track", true, "2026-09-16T13:07:00Z"),
        ];
        assert_eq!(
            vote_start(&ft, created),
            (at("2026-09-16T13:07:00Z"), "fast track label")
        );
        let mut both = ft.clone();
        both.push(label("vote-in-progress", true, "2026-09-17T00:00:00Z"));
        assert_eq!(vote_start(&both, created).1, "vote-in-progress label");
        assert_eq!(vote_start(&[], created), (created, "ticket creation"));
    }

    #[test]
    fn classify_follows_the_policy_clock() {
        let start = at("2026-09-16T13:07:00Z");
        let t = |plus: usize, minus: usize| Tally {
            plus: (0..plus).map(|i| format!("p{i}")).collect(),
            minus: (0..minus).map(|i| format!("m{i}")).collect(),
            ..Tally::default()
        };
        // #3685 on meeting day: +5, fast track, week not up.
        let v = classify(&t(5, 0), true, start, at("2026-09-22T12:00:00Z"));
        assert_eq!(v.outcome, Outcome::Waiting);
        assert_eq!(v.decidable_at, Some(at("2026-09-23T13:07:00Z")));
        assert!(
            v.detail
                .starts_with("2 more +1 approves it now under Fast Track"),
            "{}",
            v.detail
        );
        // +5 concludes at the week's end whatever Fast Track needs.
        assert_eq!(v.votes_needed, 0);
        // +3 before the week is up needs nothing more either, +1 does.
        let early = at("2026-09-22T12:00:00Z");
        assert_eq!(classify(&t(3, 0), false, start, early).votes_needed, 0);
        assert_eq!(classify(&t(1, 0), false, start, early).votes_needed, 2);
        // Fast Track reaches +7.
        assert_eq!(
            classify(&t(7, 0), true, start, at("2026-09-17T00:00:00Z")).outcome,
            Outcome::Approved
        );
        // Same +7 without Fast Track waits for the week.
        assert_eq!(
            classify(&t(7, 0), false, start, at("2026-09-17T00:00:00Z")).outcome,
            Outcome::Waiting
        );
        // A -1 goes to the meeting whatever the count.
        let v = classify(&t(6, 1), true, start, at("2026-09-17T00:00:00Z"));
        assert_eq!(v.outcome, Outcome::Meeting);
        assert!(v.detail.contains("drops it from Fast Track"));
        // After a week: +3 approves, +2 gets a second week.
        assert_eq!(
            classify(&t(3, 0), false, start, at("2026-09-24T00:00:00Z")).outcome,
            Outcome::Approved
        );
        let v = classify(&t(2, 0), false, start, at("2026-09-24T00:00:00Z"));
        assert_eq!(
            (v.outcome, v.decidable_at),
            (Outcome::Waiting, Some(at("2026-09-30T13:07:00Z")))
        );
        // A second week with +2 already in hand needs no more votes.
        assert_eq!(v.votes_needed, 0);
        // After two weeks: one +1 approves, none rejects, seven -1 reject.
        assert_eq!(
            classify(&t(1, 0), false, start, at("2026-10-01T00:00:00Z")).outcome,
            Outcome::Approved
        );
        assert_eq!(
            classify(&t(0, 0), false, start, at("2026-10-01T00:00:00Z")).outcome,
            Outcome::Rejected
        );
        assert_eq!(
            classify(&t(0, 7), false, start, at("2026-09-24T00:00:00Z")).outcome,
            Outcome::Rejected
        );
    }

    #[test]
    fn reminder_tracks_the_48_hours_and_a_members_reminder_comment() {
        let start = at("2026-09-16T13:07:00Z");
        let m = members(&["salimma", "zbyszek"]);
        let events = vec![comment(
            "zbyszek",
            "I sent a mail about the plan",
            "2026-09-16T15:41:00Z",
        )];
        assert_eq!(
            reminder(&events, &m, start, at("2026-09-17T00:00:00Z")),
            Reminder::Due {
                at: at("2026-09-18T13:07:00Z")
            }
        );
        assert_eq!(
            reminder(&events, &m, start, at("2026-09-22T10:00:00Z")),
            Reminder::Overdue {
                since: at("2026-09-18T13:07:00Z")
            }
        );
        let mut sent = events.clone();
        sent.push(comment(
            "salimma",
            "Reminder sent - https://lists.example/…",
            "2026-09-22T10:37:00Z",
        ));
        assert_eq!(
            reminder(&sent, &m, start, at("2026-09-22T12:00:00Z")),
            Reminder::Sent {
                at: at("2026-09-22T10:37:00Z"),
                by: "salimma".into()
            }
        );
    }

    #[test]
    fn render_hints_when_the_vote_outlives_the_meeting() {
        let report = VoteReport {
            ticket: Ticket {
                number: 3685,
                title: "[FastTrack] Upgrade to libxml2-2.15.4 in F45".into(),
                url: "https://forge.example/3685".into(),
                decision: None,
                repo: None,
                pull: false,
                created: None,
                updated: None,
            },
            fast_track: true,
            start: at("2026-09-16T13:07:00Z"),
            start_basis: "fast track label",
            votes: Tally {
                plus: members(&["kevin", "zbyszek"]),
                zero: members(&["decathorpe"]),
                minus: vec![],
                silent: members(&["duffy"]),
            },
            tally: "(+2, 1, -0)".into(),
            verdict: Verdict {
                outcome: Outcome::Waiting,
                detail: "5 more +1 approves it now under Fast Track".into(),
                votes_needed: 1,
                decidable_at: Some(at("2026-09-23T13:07:00Z")),
            },
            reminder: Some(Reminder::Overdue {
                since: at("2026-09-18T13:07:00Z"),
            }),
        };
        let text = render(&report, NaiveDate::from_ymd_opt(2026, 9, 22).unwrap());
        assert!(text.contains("#3685 [FastTrack] Upgrade"), "{text}");
        assert!(
            text.contains(
                "  tally (+2, 1, -0)\n    +1: kevin, zbyszek\n     0: decathorpe\n    -1: -\n"
            ),
            "{text}"
        );
        assert!(text.contains("no vote: duffy"), "{text}");
        assert!(
            text.contains("not decidable in ticket before the 2026-09-22 meeting"),
            "{text}"
        );
        assert!(
            text.contains("NEEDS VOTES: 1 more +1; not voted: duffy"),
            "{text}"
        );
        assert!(
            text.contains("48h reminder: OVERDUE since 2026-09-18 13:07 UTC"),
            "{text}"
        );
        let later = render(&report, NaiveDate::from_ymd_opt(2026, 9, 29).unwrap());
        assert!(
            later.contains("decidable in ticket on 2026-09-23, before the 2026-09-29 meeting"),
            "{later}"
        );
    }
}
