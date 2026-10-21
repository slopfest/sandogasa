// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `votes` subcommand — where every open in-ticket vote stands under
//! the FESCo ticket-vote policy
//! (<https://docs.fedoraproject.org/en-US/fesco/#ticket-votes>).
//!
//! Tickets labeled `vote-in-progress` or `fast track` are scanned;
//! the latest `+1` / `0` / `-1` from each currently serving FESCo
//! member (the `fesco` FAS group) is tallied, the clock starts at the
//! `vote-in-progress` label or, without one, at the ticket's creation,
//! and the policy's rules give a verdict:
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

/// The roster corrections, shared by every subcommand that reads
/// votes (`votes`, `agenda`, `script`, `summary`).
#[derive(clap::Args, Default)]
pub struct VoterArgs {
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
}

#[derive(clap::Args)]
pub struct VotesArgs {
    /// Meeting date the hints refer to (default: the coming Tuesday).
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub date: Option<NaiveDate>,

    /// Count these FAS users as voters instead of the fesco group
    /// (repeat/CSV).
    #[arg(long = "member", value_name = "FAS", value_delimiter = ',')]
    pub members: Vec<String>,

    #[command(flatten)]
    pub voters: VoterArgs,

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
    /// One comment carrying both a `+1` and a `-1`: a position held
    /// against a condition rather than a vote cast. Counted as
    /// neither, because taking the last token read "I'm +1 assuming
    /// this can be done quickly, but -1 if it turns out we'd have a
    /// broken package" as a flat -1.
    Unclear,
}

/// Each member's latest vote, split by value; `silent` members have
/// not voted.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Tally {
    pub plus: Vec<String>,
    pub zero: Vec<String>,
    pub minus: Vec<String>,
    /// Members whose latest comment is a [`Vote::Unclear`]. Left out
    /// of [`Tally::counts`]: the tally reports votes cast, and this
    /// is the absence of one.
    pub unclear: Vec<String>,
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
    /// Whether the ticket carries `vote-in-progress`, which is stale
    /// once the vote is decided and wants dropping with the same
    /// edit that adds `pending announcement`.
    pub vote_label: bool,
    /// A decision already written into the ticket's comments, if
    /// there is one. A ticket tagged `pending announcement` never
    /// reaches the offer at all, but one commented by hand and not
    /// yet tagged does, and must not be commented a second time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub announced: Option<String>,
    /// What each member in [`Tally::unclear`] actually wrote. The
    /// tool cannot tell a held question from a firm objection, so
    /// the sentence goes in front of the person who can.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unclear_comments: BTreeMap<String, Unclear>,
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
    let (mut plus, mut minus) = (false, false);
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with('>') || t.contains("(+") {
            continue;
        }
        for word in t.split_whitespace() {
            let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && !"+-±".contains(c));
            match w {
                "+1" => (explicit, plus) = (Some(Vote::Plus), true),
                "-1" => (explicit, minus) = (Some(Vote::Minus), true),
                "+0" | "-0" | "±0" | "+-0" | "+/-0" => explicit = Some(Vote::Zero),
                "0" => bare_zero = true,
                _ => {}
            }
        }
    }
    if plus && minus {
        return Some(Vote::Unclear);
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
            Some(Vote::Unclear) => out.unclear.push(m.clone()),
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
    // After the -1 rule, so a standing objection keeps the clearer
    // explanation; both end at the meeting either way. Before every
    // approval path, because an unresolved condition is exactly what
    // must not be approved past.
    if !tally.unclear.is_empty() {
        return verdict(
            Outcome::Meeting,
            format!(
                "unclear vote from {}: a comment carrying both a +1 and a -1 \
                 is counted as neither, and the question it leaves open is \
                 what the meeting is for",
                tally.unclear.join(", ")
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
pub fn roster(
    members: &[String],
    non_voting: &[String],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut members = if members.is_empty() {
        ensure_kerberos_ticket()?;
        sandogasa_fasjson::FasjsonClient::new()
            .group_members(FESCO_GROUP, None)?
            .into_iter()
            .map(|u| u.username)
            .collect()
    } else {
        members.to_vec()
    };
    members.retain(|m| !non_voting.contains(m));
    members.sort();
    members.dedup();
    Ok(members)
}

/// The `--ignore` / `--vote` corrections, keyed by ticket.
fn overrides(args: &VoterArgs) -> BTreeMap<u64, BTreeMap<String, Option<Vote>>> {
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
    overrides
}

/// The open vote tickets with their reports, for `agenda` and
/// `summary` to offer. Those commands stand without this, so a roster
/// or tracker failure warns and yields nothing.
pub fn open_votes(
    client: &sandogasa_forgejo::Client,
    args: &VoterArgs,
    verbose: bool,
    now: DateTime<Utc>,
) -> Vec<VoteReport> {
    let scanned = roster(&[], &args.non_voting)
        .and_then(|members| scan(client, &members, &overrides(args), verbose, now));
    scanned.unwrap_or_else(|e| {
        eprintln!(
            "warning: not checking tickets under an in-ticket vote ({e}); see `fesco-chair votes`"
        );
        Vec::new()
    })
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

/// Scan the tracker's open vote tickets and report on each.
pub fn scan(
    client: &sandogasa_forgejo::Client,
    members: &[String],
    overrides: &BTreeMap<u64, BTreeMap<String, Option<Vote>>>,
    verbose: bool,
    now: DateTime<Utc>,
) -> Result<Vec<VoteReport>, Box<dyn std::error::Error>> {
    let mut issues = Vec::new();
    // Which tickets carry `vote-in-progress`, so the chair can be
    // told to drop it: a Fast Track ticket need not have it, and the
    // issues are deduped below, which loses the query that found one.
    let mut voting = std::collections::BTreeSet::new();
    for label in [FAST_TRACK_LABEL, VOTE_LABEL] {
        if verbose {
            eprintln!("[votes] fetching open `{label}` tickets");
        }
        let found = client.repo_issues(TRACKER_OWNER, TRACKER_REPO, "open", &[label])?;
        if label == VOTE_LABEL {
            voting.extend(found.iter().map(|i| i.number));
        }
        issues.extend(found);
    }
    issues.sort_by_key(|i| i.number);
    issues.dedup_by_key(|i| i.number);
    let mut out = Vec::new();
    for issue in issues {
        if verbose {
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
        // The week runs from when the ticket was opened. The
        // `vote-in-progress` label only draws attention to a vote;
        // counting from it gave Change: Libical4 a week that had not
        // ended by the meeting, twelve days after it actually
        // approved.
        let (start, start_basis) = (created, "ticket creation");
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
        // Newest first, as extract_decision reads them.
        let bodies: Vec<&str> = comments(&events).map(|(_, b, _)| b).collect();
        let announced = sources::extract_decision(bodies.into_iter().rev());
        // The comment that could not be read as a vote, per member,
        // taking the latest as the tally does. Read off the events
        // rather than through `comments`, which drops the anchor.
        let mut unclear_comments = BTreeMap::new();
        for who in &votes.unclear {
            if let Some(e) = events.iter().rfind(|e| {
                e.kind == "comment"
                    && e.user.as_ref().is_some_and(|u| u.login == *who)
                    && parse_vote(&e.body) == Some(Vote::Unclear)
            }) {
                unclear_comments.insert(
                    who.clone(),
                    Unclear {
                        body: e.body.clone(),
                        url: e.html_url.clone(),
                    },
                );
            }
        }
        out.push(VoteReport {
            announced,
            unclear_comments,
            vote_label: voting.contains(&issue.number),
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

/// The chair's hint while a vote is still running: whether it
/// concludes before `meeting`.
fn hint(report: &VoteReport, meeting: NaiveDate) -> Option<String> {
    let at = report.verdict.decidable_at?;
    Some(if at.date_naive() > meeting {
        format!(
            "not decidable in ticket before the {meeting} meeting: let it run, or a procedural -1 puts it on the agenda"
        )
    } else {
        format!(
            "decidable in ticket on {}, before the {meeting} meeting",
            at.format("%Y-%m-%d")
        )
    })
}

/// The label edit that moves a ticket to the announced pool.
///
/// `vote-in-progress` is stale the moment the vote concludes, and it
/// is what keeps the ticket in the vote pool on the next run, so it
/// goes in the same edit that adds the announcement label. A Fast
/// Track ticket may never have carried it.
pub(crate) fn relabel(has_vote_label: bool) -> &'static str {
    if has_vote_label {
        "swap `vote-in-progress` for `pending announcement`"
    } else {
        "tag `pending announcement`"
    }
}

/// A comment that could not be read as a vote, and where to find
/// it: on a long thread the sentence alone is not enough to go back
/// to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Unclear {
    pub body: String,
    /// The comment's anchor on the issue page, empty when the API
    /// gave none.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
}

/// One line of an unclear comment, for showing at a prompt.
///
/// The first non-empty line is enough to recognise what was said;
/// the full text is a click away and the ticket is named above it.
pub fn unclear_excerpt(body: &str, width: usize) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= width {
        return line.to_string();
    }
    let kept: String = line.chars().take(width.saturating_sub(1)).collect();
    // Trimmed before the ellipsis: cutting mid-sentence often lands
    // on a space, and "turns out …" reads as a typo.
    format!("{}\u{2026}", kept.trim_end())
}

/// Move a member's unclear vote to `how`, or leave it unclear when
/// `None`, and work out the verdict again.
///
/// The same effect as `--vote N:FAS=V`, applied without a second
/// run: the tool cannot read a conditional, but the chair reading
/// the comment can, and asking them there is cheaper than asking
/// them to start over.
pub fn resolve_unclear(report: &mut VoteReport, who: &str, how: Option<Vote>, now: DateTime<Utc>) {
    let Some(at) = report.votes.unclear.iter().position(|m| m == who) else {
        return;
    };
    let Some(how) = how else { return };
    report.votes.unclear.remove(at);
    report.unclear_comments.remove(who);
    match how {
        Vote::Plus => report.votes.plus.push(who.to_string()),
        Vote::Zero => report.votes.zero.push(who.to_string()),
        Vote::Minus => report.votes.minus.push(who.to_string()),
        // Already where it belongs.
        Vote::Unclear => return,
    }
    for list in [
        &mut report.votes.plus,
        &mut report.votes.zero,
        &mut report.votes.minus,
    ] {
        list.sort();
    }
    report.tally = report.votes.counts();
    report.verdict = classify(&report.votes, report.fast_track, report.start, now);
}

/// The `--vote` flag that would reproduce a resolution.
///
/// An answer at the prompt lives only for that run — the comment it
/// settles has not changed, so the next run asks again. Printing the
/// flag means a ticket that lingers can be answered once and pasted
/// thereafter.
///
/// Round-trips through the parser `--vote` itself uses, so what is
/// printed is accepted.
pub fn vote_flag(number: u64, who: &str, how: Vote) -> String {
    let v = match how {
        Vote::Plus => "+1",
        Vote::Zero => "0",
        Vote::Minus => "-1",
        // Not reachable from a resolution, which never sets it.
        Vote::Unclear => "0",
    };
    format!("--vote {number}:{who}={v}")
}

/// Read an answer to the how-should-this-count prompt. `None` is
/// unrecognized; `Some(None)` leaves the vote unclear.
#[allow(clippy::option_option)]
pub fn parse_unclear_answer(line: &str) -> Option<Option<Vote>> {
    match line.trim() {
        "" | "l" | "leave" => Some(None),
        "+1" | "1" | "+" => Some(Some(Vote::Plus)),
        "0" => Some(Some(Vote::Zero)),
        "-1" | "-" => Some(Some(Vote::Minus)),
        _ => None,
    }
}

/// What to do with a ticket whose vote has outrun the agenda.
///
/// Both cases end at the same label and differ only in which email
/// carries the result: a vote already decided is announced beside
/// the agenda it is being offered to, and one that concludes between
/// now and the meeting is announced by the summary afterwards.
/// Neither wants the discussion slot that saying yes would buy it,
/// which is the part that is easy to get wrong -- the prompt asks
/// only whether to add it, so the alternative is never on screen.
///
/// `None` once a vote belongs at the meeting, or while it cannot
/// conclude before one: `hint` already says what to do there.
pub fn announce_hint(report: &VoteReport, meeting: NaiveDate) -> Option<String> {
    match report.verdict.outcome {
        Outcome::Approved | Outcome::Rejected => Some(format!(
            "decided: comment \u{201c}{}\u{201d}, {}, and it is announced \
             with this agenda",
            // The decision line alone, which is what the announcement
            // prints and what the tracker's own comments settle on.
            // Members prefix it variously -- "After a week: ", "This is
            // now approved by" -- and how long the vote actually ran is
            // the line above this one, so the hint does not put words
            // in the chair's mouth about it.
            decision(report)?,
            relabel(report.vote_label),
        )),
        Outcome::Waiting
            if report
                .verdict
                .decidable_at
                .is_some_and(|at| at.date_naive() <= meeting) =>
        {
            Some(format!(
                "once it concludes, {} and the post-meeting summary \
                 announces it, so leaving it off here costs no time",
                relabel(report.vote_label),
            ))
        }
        _ => None,
    }
}

/// The one-paragraph state of a vote, for the `agenda` and `summary`
/// prompts: kind and start, tally, verdict, and the hint.
pub fn brief(report: &VoteReport, meeting: NaiveDate) -> String {
    let kind = if report.fast_track {
        "Fast Track"
    } else {
        "ticket vote"
    };
    let mut text = format!(
        "{kind} since {}, tally {}\n  {}: {}",
        report.start.format("%Y-%m-%d"),
        report.tally,
        format!("{:?}", report.verdict.outcome).to_lowercase(),
        report.verdict.detail
    );
    if let Some(h) = hint(report, meeting) {
        text.push_str("\n  ");
        text.push_str(&h);
    }
    text
}

/// The decision line for a concluded vote, in the form the
/// announcement uses: `APPROVED (+5, 1, -0)`. `None` while it runs.
pub fn decision(report: &VoteReport) -> Option<String> {
    let verdict = match report.verdict.outcome {
        Outcome::Approved => "APPROVED",
        Outcome::Rejected => "REJECTED",
        Outcome::Meeting | Outcome::Waiting => return None,
    };
    Some(format!("{verdict} {}", report.tally))
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
        format!("    unclear: {}", list(&report.votes.unclear)),
        format!("    no vote: {}", list(&report.votes.silent)),
        format!(
            "  {}: {}",
            format!("{:?}", report.verdict.outcome).to_lowercase(),
            report.verdict.detail
        ),
    ];

    for (who, c) in &report.unclear_comments {
        lines.push(format!("    {who}: {}", unclear_excerpt(&c.body, 64)));
        if !c.url.is_empty() {
            lines.push(format!("      {}", c.url));
        }
    }
    if let Some(h) = hint(report, meeting) {
        lines.push(format!("  {h}"));
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
    let result = roster(&args.members, &args.voters.non_voting).and_then(|members| {
        if args.verbose {
            eprintln!("[votes] voters: {}", members.join(", "));
        }
        let client = sources::forge_client()?;
        scan(
            &client,
            &members,
            &overrides(&args.voters),
            args.verbose,
            now,
        )
        .map(|reports| (members, reports))
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

    fn event(v: serde_json::Value) -> TimelineEvent {
        serde_json::from_value(v).unwrap()
    }

    fn comment(login: &str, body: &str, when: &str) -> TimelineEvent {
        event(serde_json::json!({
            "type": "comment", "body": body, "user": {"login": login}, "created_at": when
        }))
    }

    fn comment_at(login: &str, body: &str, when: &str, url: &str) -> TimelineEvent {
        event(serde_json::json!({
            "type": "comment", "body": body, "user": {"login": login},
            "created_at": when, "html_url": url
        }))
    }

    fn label(name: &str, added: bool, when: &str) -> TimelineEvent {
        event(serde_json::json!({
            "type": "label", "body": if added { "1" } else { "" },
            "created_at": when, "label": {"name": name}
        }))
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
    fn an_unclear_comment_is_found_with_its_anchor() {
        // The API gives the anchor outright, which is what makes a
        // long thread navigable from the prompt.
        let events = [
            comment_at(
                "decathorpe",
                "In general I would be +1 here",
                "2026-10-18T10:00:00Z",
                "u/1",
            ),
            comment_at(
                "decathorpe",
                "I'm +1 assuming X, but -1 if Y",
                "2026-10-19T10:00:00Z",
                "https://forge.example/issues/3696#issuecomment-1367373",
            ),
        ];
        let who = "decathorpe";
        let found = events
            .iter()
            .rfind(|e| {
                e.kind == "comment"
                    && e.user.as_ref().is_some_and(|u| u.login == *who)
                    && parse_vote(&e.body) == Some(Vote::Unclear)
            })
            .expect("the latest unclear comment");
        // The earlier firm +1 is not it; the later ambiguous one is.
        assert_eq!(found.body, "I'm +1 assuming X, but -1 if Y");
        assert_eq!(
            found.html_url,
            "https://forge.example/issues/3696#issuecomment-1367373"
        );
    }

    #[test]
    fn an_unclear_excerpt_is_one_line_and_bounded() {
        let real = "I'm +1 assuming this *can* be done quickly, but -1 if it \
                    turns out we'd have a broken gedit-plugins package for the \
                    lifetime of F45.";
        let short = unclear_excerpt(real, 40);
        assert!(short.chars().count() <= 40, "{short}");
        assert!(short.ends_with('\u{2026}'), "{short}");
        // No space left dangling before the ellipsis.
        assert!(!short.contains(" \u{2026}"), "{short}");
        assert!(short.starts_with("I'm +1 assuming"), "{short}");
        // A short comment is given whole, with no ellipsis.
        assert_eq!(unclear_excerpt("+1 or -1", 40), "+1 or -1");
        // Leading blank lines are skipped, and only one line shown.
        assert_eq!(unclear_excerpt("\n\n  +1 or -1\nmore\n", 40), "+1 or -1");
        assert_eq!(unclear_excerpt("", 40), "");
    }

    #[test]
    fn resolving_an_unclear_vote_recounts_and_rejudges() {
        let roster = members(&["ngompa", "salimma", "decathorpe"]);
        let votes = tally(
            [
                ("ngompa", "+1"),
                ("salimma", "+1"),
                ("decathorpe", "I'm +1 assuming X, but -1 if Y"),
            ]
            .into_iter(),
            &roster,
            &BTreeMap::new(),
        );
        let start = at("2026-10-18T12:00:00Z");
        let now = at("2026-10-19T12:00:00Z");
        let mut report = VoteReport {
            ticket: Ticket {
                number: 3696,
                title: "t".into(),
                url: "u".into(),
                decision: None,
                repo: None,
                pull: false,
                created: None,
                updated: None,
            },
            fast_track: true,
            vote_label: true,
            announced: None,
            unclear_comments: BTreeMap::from([(
                "decathorpe".to_string(),
                Unclear {
                    body: "I'm +1 assuming X, but -1 if Y".to_string(),
                    url: "https://forge.example/fesco/tickets/issues/3696#issuecomment-1367373"
                        .to_string(),
                },
            )]),
            start,
            start_basis: "ticket creation",
            tally: votes.counts(),
            verdict: classify(&votes, true, start, now),
            votes,
            reminder: None,
        };
        assert_eq!(report.tally, "(+2, 0, -0)");
        assert_eq!(report.verdict.outcome, Outcome::Meeting);

        // Leaving it unclear changes nothing at all.
        resolve_unclear(&mut report, "decathorpe", None, now);
        assert_eq!(report.votes.unclear, members(&["decathorpe"]));
        assert_eq!(report.tally, "(+2, 0, -0)");

        // Counting it as the -1 he might have meant keeps the ticket
        // at the meeting, but now on a recorded objection.
        let mut as_minus = report;
        resolve_unclear(&mut as_minus, "decathorpe", Some(Vote::Minus), now);
        assert!(as_minus.votes.unclear.is_empty());
        assert!(as_minus.unclear_comments.is_empty());
        assert_eq!(as_minus.tally, "(+2, 0, -1)");
        assert_eq!(as_minus.verdict.outcome, Outcome::Meeting);
        assert!(as_minus.verdict.detail.starts_with("-1 from decathorpe"));
    }

    #[test]
    fn the_printed_vote_flag_is_one_the_parser_accepts() {
        // What is printed has to be pasteable, so it round-trips
        // through the parser `--vote` itself uses.
        for how in [Vote::Plus, Vote::Zero, Vote::Minus] {
            let flag = vote_flag(3696, "decathorpe", how);
            let arg = flag.strip_prefix("--vote ").expect("the flag's value");
            assert_eq!(
                parse_override(arg),
                Ok((3696, "decathorpe".to_string(), how)),
                "{flag}"
            );
        }
        assert_eq!(
            vote_flag(3696, "decathorpe", Vote::Minus),
            "--vote 3696:decathorpe=-1"
        );
    }

    #[test]
    fn an_unclear_answer_is_read_strictly() {
        assert_eq!(parse_unclear_answer(""), Some(None));
        assert_eq!(parse_unclear_answer("l"), Some(None));
        assert_eq!(parse_unclear_answer("+1"), Some(Some(Vote::Plus)));
        assert_eq!(parse_unclear_answer(" 1 "), Some(Some(Vote::Plus)));
        assert_eq!(parse_unclear_answer("0"), Some(Some(Vote::Zero)));
        assert_eq!(parse_unclear_answer("-1"), Some(Some(Vote::Minus)));
        // Enter leaves it unclear, so a typo must re-ask rather than
        // silently counting a vote nobody cast.
        assert_eq!(parse_unclear_answer("maybe"), None);
        assert_eq!(parse_unclear_answer("+2"), None);
    }

    #[test]
    fn a_comment_holding_both_signs_is_not_a_vote() {
        // decathorpe on #3696, verbatim. Taking the last token read
        // this as a flat -1 and sent the ticket to the meeting as a
        // recorded objection, which is not what it says.
        assert_eq!(
            parse_vote(
                "I'm +1 assuming this *can* be done quickly, but -1 if it \
                 turns out we'd have a broken gedit-plugins package for the \
                 lifetime of F45."
            ),
            Some(Vote::Unclear)
        );
        // His earlier comment holds one sign and is a vote.
        assert_eq!(
            parse_vote("In general I would be +1 here, though I'd like to know how"),
            Some(Vote::Plus)
        );
        // A quoted pair is somebody else's words, and a tally line is
        // a record, so neither turns a comment unclear.
        assert_eq!(parse_vote("> I'm +1 ... but -1 if\n\n+1"), Some(Vote::Plus));
        assert_eq!(parse_vote("After a week: APPROVED (+3, 0, 0)"), None);
    }

    #[test]
    fn an_unclear_vote_is_uncounted_and_sends_the_ticket_to_the_meeting() {
        let roster = members(&["ngompa", "salimma", "decathorpe"]);
        let t = tally(
            [
                ("ngompa", "+1"),
                ("salimma", "+1"),
                ("decathorpe", "I'm +1 assuming X, but -1 if Y"),
            ]
            .into_iter(),
            &roster,
            &BTreeMap::new(),
        );
        assert_eq!(t.unclear, members(&["decathorpe"]));
        // Counted as neither, so the tally reports the two real +1s.
        assert_eq!(t.counts(), "(+2, 0, -0)");

        // It still reaches the agenda, on the honest grounds.
        let start = at("2026-10-18T12:00:00Z");
        let v = classify(&t, true, start, at("2026-10-19T12:00:00Z"));
        assert_eq!(v.outcome, Outcome::Meeting);
        assert!(v.detail.contains("decathorpe"), "{}", v.detail);
        assert!(v.detail.contains("unclear"), "{}", v.detail);

        // The chair can resolve it by hand without editing the ticket.
        let fixed = tally(
            [("decathorpe", "I'm +1 assuming X, but -1 if Y")].into_iter(),
            &roster,
            &BTreeMap::from([("decathorpe".to_string(), Some(Vote::Minus))]),
        );
        assert_eq!(fixed.minus, members(&["decathorpe"]));
        assert!(fixed.unclear.is_empty());
    }

    #[test]
    fn a_standing_minus_one_keeps_its_own_explanation() {
        // Both route to the meeting; the -1 is the clearer signal
        // and is what the chair is told.
        let roster = members(&["a", "b"]);
        let t = tally(
            [("a", "-1"), ("b", "+1 if X but -1 if Y")].into_iter(),
            &roster,
            &BTreeMap::new(),
        );
        let v = classify(
            &t,
            false,
            at("2026-10-10T12:00:00Z"),
            at("2026-10-19T12:00:00Z"),
        );
        assert_eq!(v.outcome, Outcome::Meeting);
        assert!(v.detail.starts_with("-1 from a"), "{}", v.detail);
    }

    #[test]
    fn a_decision_already_in_the_comments_is_found() {
        // A ticket commented by hand but not yet tagged still
        // reaches the offer, and must not be commented twice.
        let events = vec![
            comment("zbyszek", "+1", "2026-10-02T09:00:00Z"),
            comment("salimma", "APPROVED (+4, 0, -0)", "2026-10-08T09:00:00Z"),
        ];
        let bodies: Vec<&str> = comments(&events).map(|(_, b, _)| b).collect();
        assert_eq!(
            sources::extract_decision(bodies.into_iter().rev()).as_deref(),
            Some("APPROVED (+4, 0, -0)")
        );
        // A ticket whose vote is merely running has none.
        let running = vec![comment("kevin", "+1", "2026-10-02T09:00:00Z")];
        let bodies: Vec<&str> = comments(&running).map(|(_, b, _)| b).collect();
        assert_eq!(sources::extract_decision(bodies.into_iter().rev()), None);
    }

    #[test]
    fn label_added_at_takes_the_last_addition_and_ignores_removals() {
        // Fast Track's clock is the only thing still read from a
        // label, and a label can go on, come off and go back on --
        // #3690 carried two `vote-in-progress` events that way.
        let events = vec![
            label("fast track", true, "2026-09-16T13:07:00Z"),
            label("fast track", false, "2026-09-17T09:00:00Z"),
            label("fast track", true, "2026-09-18T10:00:00Z"),
        ];
        assert_eq!(
            label_added_at(&events, "fast track"),
            Some(at("2026-09-18T10:00:00Z"))
        );
        // A label only ever removed was never added.
        let off = vec![label("meeting", false, "2026-09-17T09:00:00Z")];
        assert_eq!(label_added_at(&off, "meeting"), None);
        assert_eq!(label_added_at(&events, "meeting"), None);
    }

    #[test]
    fn the_week_runs_from_when_the_ticket_was_opened() {
        // Change: Libical4 — filed 2026-10-01, labelled 2026-10-13,
        // +4 and no -1. Counting from the label gave it a week
        // ending 13:38 on the day of the meeting, so the agenda said
        // "approves 2026-10-20" for a vote that had in fact approved
        // on 2026-10-08. The label only flags a vote; it does not
        // start one.
        let roster = members(&["a", "b", "c", "d"]);
        let t = tally(
            [("a", "+1"), ("b", "+1"), ("c", "+1"), ("d", "+1")].into_iter(),
            &roster,
            &BTreeMap::new(),
        );
        let opened = at("2026-10-01T11:40:12Z");
        let labelled = at("2026-10-13T13:38:33Z");
        let meeting_morning = at("2026-10-20T09:00:00Z");
        assert_eq!(
            classify(&t, false, opened, meeting_morning).outcome,
            Outcome::Approved
        );
        assert_eq!(
            classify(&t, false, opened, at("2026-10-08T12:00:00Z")).outcome,
            Outcome::Approved
        );
        // What the label-based start produced, for the record.
        assert_eq!(
            classify(&t, false, labelled, meeting_morning).outcome,
            Outcome::Waiting
        );
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
                unclear: vec![],
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

    /// A report in one outcome, for the announce-hint tests.
    fn outcome_report(outcome: Outcome, decidable: Option<&str>) -> VoteReport {
        VoteReport {
            ticket: Ticket {
                number: 3691,
                title: "t".into(),
                url: "u".into(),
                decision: None,
                repo: None,
                pull: false,
                created: None,
                updated: None,
            },
            fast_track: false,
            vote_label: true,
            announced: None,
            unclear_comments: BTreeMap::new(),
            start: at("2026-10-13T13:00:00Z"),
            start_basis: "ticket creation",
            votes: Tally::default(),
            tally: "(+4, 0, -0)".into(),
            verdict: Verdict {
                outcome,
                detail: "d".into(),
                votes_needed: 0,
                decidable_at: decidable.map(at),
            },
            reminder: None,
        }
    }

    #[test]
    fn the_vote_report_never_ends_a_line_in_whitespace() {
        let meeting = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        // Each outcome prints a different hint block, and the
        // padded vote lines ("    -1: -") are where a stray space
        // would hide.
        for outcome in [Outcome::Approved, Outcome::Meeting, Outcome::Waiting] {
            let report = outcome_report(outcome, Some("2026-10-20T13:00:00Z"));
            crate::sources::assert_no_trailing_whitespace(
                "votes render",
                &render(&report, meeting),
            );
            crate::sources::assert_no_trailing_whitespace("votes brief", &brief(&report, meeting));
        }
    }

    #[test]
    fn a_decided_vote_is_pointed_at_the_label() {
        let meeting = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        for outcome in [Outcome::Approved, Outcome::Rejected] {
            let report = outcome_report(outcome, None);
            let hint =
                announce_hint(&report, meeting).expect("a decided vote has somewhere else to go");
            assert!(hint.contains("pending announcement"), "{hint}");
            // The comment to post is spelled out, and is the same
            // decision line the announcement prints.
            let decision = decision(&report).expect("a decided vote has a decision line");
            assert!(hint.contains(&decision), "{hint} / {decision}");
            // How long it ran is the brief's business, not a claim
            // the hint makes on the chair's behalf.
            assert!(!hint.contains("After a week"), "{hint}");
            // The stale label goes in the same edit, when it is there
            // at all -- a Fast Track ticket may never have carried it.
            assert!(hint.contains("swap `vote-in-progress`"), "{hint}");
            let mut bare = outcome_report(outcome, None);
            bare.vote_label = false;
            let hint = announce_hint(&bare, meeting).expect("still decided");
            assert!(hint.contains("tag `pending announcement`"), "{hint}");
            assert!(!hint.contains("vote-in-progress"), "{hint}");
        }
    }

    #[test]
    fn a_vote_concluding_before_the_meeting_is_told_the_summary_catches_it() {
        // #3691's case: it approves the morning of the meeting, so
        // it cannot be announced with the agenda and does not need a
        // slot either -- leaving it off loses no week.
        let meeting = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let hint = announce_hint(
            &outcome_report(Outcome::Waiting, Some("2026-10-20T09:00:00Z")),
            meeting,
        )
        .expect("a vote concluding in time is announced by the summary");
        assert!(hint.contains("summary"), "{hint}");

        // One that cannot conclude in time is left to `hint`, which
        // already says to let it run or force it with a -1.
        assert_eq!(
            announce_hint(
                &outcome_report(Outcome::Waiting, Some("2026-10-27T09:00:00Z")),
                meeting
            ),
            None
        );
        // A ticket already bound for the meeting wants its slot.
        assert_eq!(
            announce_hint(&outcome_report(Outcome::Meeting, None), meeting),
            None
        );
    }

    #[test]
    fn brief_and_decision_summarize_a_report() {
        let mut report = VoteReport {
            ticket: Ticket {
                number: 3685,
                title: "t".into(),
                url: "u".into(),
                decision: None,
                repo: None,
                pull: false,
                created: None,
                updated: None,
            },
            fast_track: true,
            vote_label: true,
            announced: None,
            unclear_comments: BTreeMap::new(),
            start: at("2026-09-16T13:07:00Z"),
            start_basis: "ticket creation",
            votes: Tally::default(),
            tally: "(+5, 1, -0)".into(),
            verdict: Verdict {
                outcome: Outcome::Waiting,
                detail: "d".into(),
                votes_needed: 0,
                decidable_at: Some(at("2026-09-23T13:07:00Z")),
            },
            reminder: None,
        };
        let meeting = NaiveDate::from_ymd_opt(2026, 9, 22).unwrap();
        assert_eq!(
            brief(&report, meeting),
            "Fast Track since 2026-09-16, tally (+5, 1, -0)\n  waiting: d\n  not decidable in ticket before the 2026-09-22 meeting: let it run, or a procedural -1 puts it on the agenda"
        );
        assert_eq!(decision(&report), None);
        report.verdict.outcome = Outcome::Approved;
        report.verdict.decidable_at = None;
        assert_eq!(decision(&report).as_deref(), Some("APPROVED (+5, 1, -0)"));
        assert_eq!(
            brief(&report, meeting),
            "Fast Track since 2026-09-16, tally (+5, 1, -0)\n  approved: d"
        );
    }

    #[test]
    fn render_shows_an_unclear_comment_and_where_to_find_it() {
        let roster = members(&["ngompa", "decathorpe"]);
        let votes = tally(
            [("ngompa", "+1"), ("decathorpe", "+1 if X but -1 if Y")].into_iter(),
            &roster,
            &BTreeMap::new(),
        );
        let start = at("2026-10-18T12:00:00Z");
        let now = at("2026-10-19T12:00:00Z");
        let report = VoteReport {
            ticket: Ticket {
                number: 3696,
                title: "[FastTrack] gedit exception".into(),
                url: "https://forge.example/3696".into(),
                decision: None,
                repo: None,
                pull: false,
                created: None,
                updated: None,
            },
            fast_track: true,
            vote_label: true,
            announced: None,
            unclear_comments: BTreeMap::from([(
                "decathorpe".to_string(),
                Unclear {
                    body: "+1 if X but -1 if Y".to_string(),
                    url: "https://forge.example/3696#issuecomment-1367373".to_string(),
                },
            )]),
            start,
            start_basis: "ticket creation",
            tally: votes.counts(),
            verdict: classify(&votes, true, start, now),
            votes,
            reminder: None,
        };
        let out = render(&report, NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        assert!(out.contains("unclear: decathorpe"), "{out}");
        assert!(out.contains("+1 if X but -1 if Y"), "{out}");
        assert!(out.contains("#issuecomment-1367373"), "{out}");
        // The uncounted vote is absent from the three vote lines.
        assert!(out.contains("    +1: ngompa\n"), "{out}");
        assert!(out.contains("    -1: -\n"), "{out}");

        // With no anchor, no broken link is printed.
        let mut bare = report;
        bare.unclear_comments
            .get_mut("decathorpe")
            .unwrap()
            .url
            .clear();
        let out = render(&bare, NaiveDate::from_ymd_opt(2026, 10, 20).unwrap());
        assert!(out.contains("+1 if X but -1 if Y"), "{out}");
        assert!(!out.contains("issuecomment"), "{out}");
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
            vote_label: true,
            announced: None,
            unclear_comments: BTreeMap::new(),
            start: at("2026-09-16T13:07:00Z"),
            start_basis: "ticket creation",
            votes: Tally {
                plus: members(&["kevin", "zbyszek"]),
                zero: members(&["decathorpe"]),
                minus: vec![],
                unclear: vec![],
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
