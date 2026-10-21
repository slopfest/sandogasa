// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `agenda` subcommand — compose the meeting announcement email.
//!
//! Follows the template on
//! <https://fedoraproject.org/wiki/FESCo_meeting_process>: tickets
//! labeled `pending announcement` land in "Discussed and Voted in the
//! Ticket" (with a DECISION placeholder for the chair to fill in),
//! `meeting`-labeled tickets split into Followups (already discussed
//! at a previous meeting, inferred from recent meetbot minutes) and
//! New business.

use std::process::ExitCode;

use chrono::{NaiveDate, Utc};

use crate::sources::{self, Sections};

#[derive(clap::Args)]
pub struct AgendaArgs {
    /// Meeting date (default: the coming Tuesday).
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub date: Option<NaiveDate>,

    /// Show the decision comment and label edit without making them.
    #[arg(long)]
    pub dry_run: bool,

    /// Force ticket(s) into Discussed and Voted (repeat/CSV).
    #[arg(long, value_name = "N", value_delimiter = ',')]
    pub voted: Vec<u64>,

    /// Force ticket(s) into Followups (repeat/CSV).
    #[arg(long, value_name = "N", value_delimiter = ',')]
    pub followup: Vec<u64>,

    /// Force ticket(s) into New business (repeat/CSV).
    #[arg(long = "new", value_name = "N", value_delimiter = ',')]
    pub new_business: Vec<u64>,

    /// Add fesco/docs issue/PR(s) to the agenda (repeat/CSV).
    #[arg(long, value_name = "N", value_delimiter = ',')]
    pub docs: Vec<u64>,

    #[command(flatten)]
    pub voters: crate::votes::VoterArgs,

    /// Past meetings scanned for followups (default 12).
    #[arg(
        long,
        value_name = "N",
        default_value = "12",
        hide_default_value = true
    )]
    pub history: usize,

    /// Machine-readable JSON output.
    #[arg(long)]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(serde::Serialize)]
struct AgendaJson<'a> {
    date: String,
    to: &'static str,
    subject: String,
    sections: &'a Sections,
    /// Open fesco/docs items not selected for the agenda (candidates
    /// for `--docs`).
    docs_open: &'a [sources::Ticket],
    /// Tickets under an in-ticket vote left off the agenda (candidates
    /// for `--followup` / `--new`).
    votes_open: &'a [sources::Ticket],
    body: String,
}

pub fn run(args: &AgendaArgs) -> ExitCode {
    let (state, votes_open, votes_taken) = match assemble(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Persist the assembled agenda so `script` can replay these
    // decisions on meeting day without re-asking; `summary` clears it.
    crate::state::save(&state);
    let crate::state::AgendaState {
        date,
        sections,
        docs_open,
    } = state;
    let body = render_body(date, &sections);
    if args.json {
        let out = AgendaJson {
            date: date.to_string(),
            to: sources::ANNOUNCE_TO,
            subject: subject(date),
            sections: &sections,
            docs_open: &docs_open,
            votes_open: &votes_open,
            body,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    } else {
        print!(
            "To: {}\nSubject: {}\n\n{body}",
            sources::ANNOUNCE_TO,
            subject(date)
        );
        eprintln!(
            "\nafter sending: take the archive link and run `fesco-chair \
             announced <link>`, which comments it on each announced ticket, \
             untags `pending announcement` and closes it with the status it \
             was decided with"
        );
        if !votes_taken.is_empty() {
            let list: Vec<String> = votes_taken.iter().map(|n| format!("#{n}")).collect();
            eprintln!(
                "still to do by hand on {}: tag `meeting` and comment \"{}\" \
                 (see the wiki's pre-meeting list)",
                list.join(", "),
                crate::announce::meeting_comment(date),
            );
        }
    }
    ExitCode::SUCCESS
}

/// The announcement subject line.
pub fn subject(date: NaiveDate) -> String {
    format!("Schedule for Tuesday's FESCo Meeting ({date})")
}

/// The meeting date these args target: `--date`, or the coming
/// Tuesday.
pub fn target_date(args: &AgendaArgs) -> NaiveDate {
    args.date
        .unwrap_or_else(|| sources::next_tuesday(chrono::Local::now().date_naive()))
}

/// Whether any per-ticket override flag was given — a signal the user
/// wants to re-decide, so saved agenda state should not short-circuit
/// the run.
pub fn has_overrides(args: &AgendaArgs) -> bool {
    !(args.voted.is_empty()
        && args.followup.is_empty()
        && args.new_business.is_empty()
        && args.docs.is_empty())
}

/// Fetch the ticket pools and split them into sections. Also returns
/// the tickets under an in-ticket vote left off the agenda, and the
/// numbers of those taken onto it (which still need the `meeting`
/// label on the tracker — nothing here writes to it); the open
/// fesco/docs items not put on the agenda travel in the state.
/// Shared with the `script` subcommand, which runs the same
/// classification.
#[allow(clippy::type_complexity)]
pub fn assemble(
    args: &AgendaArgs,
) -> Result<(crate::state::AgendaState, Vec<sources::Ticket>, Vec<u64>), Box<dyn std::error::Error>>
{
    let date = target_date(args);
    let interactive = !args.json && std::io::IsTerminal::is_terminal(&std::io::stdin());

    let client = sources::forge_client()?;
    if args.verbose {
        eprintln!("[agenda] fetching '{}' tickets", sources::PENDING_LABEL);
    }
    let mut voted = sources::pending_tickets(&client)?;
    if args.verbose {
        eprintln!("[agenda] fetching '{}' tickets", sources::MEETING_LABEL);
    }
    let mut meeting: Vec<sources::Ticket> = client
        .repo_issues(
            sources::TRACKER_OWNER,
            sources::TRACKER_REPO,
            "open",
            &[sources::MEETING_LABEL],
        )?
        .into_iter()
        .map(Into::into)
        .collect();

    // Override flags may name tickets carrying neither label; fetch
    // those individually so they can still be placed.
    let known: std::collections::BTreeSet<u64> =
        voted.iter().chain(&meeting).map(|t| t.number).collect();
    for &number in args
        .voted
        .iter()
        .chain(&args.followup)
        .chain(&args.new_business)
    {
        if !known.contains(&number) {
            meeting.push(
                client
                    .issue(sources::TRACKER_OWNER, sources::TRACKER_REPO, number)?
                    .into(),
            );
        }
    }

    // Tickets under an in-ticket vote sit between the two pools: one
    // with a standing -1 belongs on the agenda (default yes), one that
    // will not conclude before the meeting is the chair's call
    // (default no). Accepted tickets join the meeting pool, so the
    // followup inference below places them.
    let mut votes_open = Vec::new();
    let mut votes_taken = Vec::new();
    for mut report in crate::votes::open_votes(&client, &args.voters, args.verbose, Utc::now()) {
        let number = report.ticket.number;
        if voted.iter().chain(&meeting).any(|t| t.number == number) {
            continue;
        }
        // An unclear vote is settled before anything is judged on
        // it: the verdict, the hint and the offer all read from the
        // tally, so asking afterwards would mean asking again.
        if interactive {
            eprintln!(
                "\n{} \u{201c}{}\u{201d}\n  {}",
                report.ticket.label(),
                report.ticket.title,
                report.ticket.url,
            );
            resolve_unclear_votes(&mut report)?;
        }
        if interactive {
            eprintln!("  {}", crate::votes::brief(&report, date));
            if let Some(hint) = crate::votes::announce_hint(&report, date) {
                eprintln!("  {hint}");
            }
        }
        // A decided vote wants announcing, not a discussion slot, so
        // the edit it needs is offered before the agenda question is
        // put at all -- asking that first invites a yes that sends an
        // already-decided ticket to the meeting. Taking the offer
        // moves the ticket into this very announcement's "Discussed
        // and Voted in the Ticket" section rather than the next
        // one's; declining falls through to the agenda question.
        if interactive && let Some(decision) = crate::votes::decision(&report) {
            match crate::announce::offer(
                &client,
                number,
                &decision,
                report.announced.as_deref(),
                report.vote_label,
                args.dry_run,
            ) {
                Ok(crate::announce::Announced::Done(text)) => {
                    let mut ticket = report.ticket;
                    ticket.decision = Some(text);
                    voted.push(ticket);
                    continue;
                }
                Ok(crate::announce::Announced::Skipped) => {}
                Err(e) => eprintln!("  not announced: {e}"),
            }
        }
        let take = interactive
            && sandogasa_cli::confirm(
                "add to the agenda?",
                report.verdict.outcome == crate::votes::Outcome::Meeting,
            )?;
        if take {
            // The tracker shows only the vote label on this ticket,
            // so the agenda it is now on is invisible there until
            // `meeting` goes on. Offered rather than assumed: the
            // label is what the pre-meeting list is built from.
            let tagged = interactive
                && match crate::announce::offer_meeting(&client, number, date, args.dry_run) {
                    Ok(done) => done,
                    Err(e) => {
                        eprintln!("  not tagged: {e}");
                        false
                    }
                };
            if !tagged {
                votes_taken.push(number);
            }
            meeting.push(report.ticket);
        } else {
            votes_open.push(report.ticket);
        }
    }
    if !votes_open.is_empty() && !interactive {
        eprintln!(
            "note: {} ticket(s) under an in-ticket vote not on the agenda; \
             add with --followup / --new <N,...>, see `fesco-chair votes`",
            votes_open.len()
        );
    }

    // Followup inference is best-effort: without meetbot everything
    // defaults to New business and the chair rearranges (or uses the
    // override flags).
    let past = match sources::past_ticket_numbers(
        &sandogasa_meetbot::Meetbot::new(),
        &sources::http_client(),
        date,
        args.history,
        args.verbose,
    ) {
        Ok(past) => past,
        Err(e) => {
            eprintln!(
                "warning: could not scan past meetings ({e}); listing every \
                 meeting ticket under New business — move followups with \
                 --followup <N,...>"
            );
            Default::default()
        }
    };

    let mut sections = sources::split_sections(
        voted,
        meeting,
        &past,
        &args.voted,
        &args.followup,
        &args.new_business,
    );

    // Offer the open fesco/docs issues and PRs onto the agenda (the
    // wiki's pre-meeting step 3): --docs selections go straight in,
    // the rest are prompted for one by one on a terminal (default
    // no). Selected items append to New business, after the tracker
    // tickets. Docs being unreachable only costs this offer.
    let mut docs_open = Vec::new();
    match sources::fetch_docs_items(&client) {
        Ok(items) => {
            let (selected, rest) = sources::partition_forced(items, &args.docs);
            let mut selected = selected;
            for item in rest {
                if interactive {
                    eprintln!(
                        "\n{} \u{201c}{}\u{201d}\n  {}\n  opened {}, last updated {}",
                        item.label(),
                        item.title,
                        item.url,
                        item.created.as_deref().unwrap_or("?"),
                        item.updated.as_deref().unwrap_or("?")
                    );
                }
                let take = interactive && sources::confirm_default_no("add to the agenda?")?;
                if take {
                    selected.push(item);
                } else {
                    docs_open.push(item);
                }
            }
            if !docs_open.is_empty() && !interactive {
                eprintln!(
                    "note: {} open fesco/docs item(s) not on the agenda; \
                     add with --docs <N,...>",
                    docs_open.len()
                );
            }
            sections.new_business.extend(selected);
        }
        Err(e) => eprintln!("warning: could not fetch fesco/docs items ({e})"),
    }

    sources::fill_decisions(&client, &mut sections.voted, args.verbose);

    let state = crate::state::AgendaState {
        date,
        sections,
        docs_open,
    };
    Ok((state, votes_open, votes_taken))
}

/// Render the announcement body (everything below the Subject line),
/// following the wiki template. The "Discussed and Voted in the
/// Ticket" section is omitted when empty (matching the wiki's
/// sample); Followups and New business always appear so the chair
/// can slot in late additions, as does Council happenings — the
/// standing slot for the FESCo Council representative, which the
/// chair fills in by hand.
pub fn render_body(date: NaiveDate, sections: &Sections) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "Following is the list of topics that will be discussed in the FESCo\n\
         meeting Tuesday at 18:00 Europe/London in #meeting:fedoraproject.org\n\
         on Matrix.\n\
         \n\
         To convert Europe/London (UTC/UTC+1) to your local time, take a look at\n\
         \x20 https://fedoraproject.org/wiki/UTCHowto\n\
         \n\
         or run:\n\
         \x20 date -d 'TZ=\"Europe/London\" {date} 18:00'\n\
         \n\
         Links to all issues to be discussed can be found at:\n\
         {}",
        sources::AGENDA_URL
    );
    if !sections.voted.is_empty() {
        let _ = writeln!(o, "\n= Discussed and Voted in the Ticket =");
        for t in &sections.voted {
            let decision = t.decision.as_deref().unwrap_or("DECISION (+X, Y, -Z)");
            // Entries lead with #NNNN like the other sections (the
            // wiki template omits it here, but consistency wins).
            let _ = writeln!(o, "\n{}\n{decision}", sources::entry(t));
        }
    }
    let _ = writeln!(o, "\n= Followups =");
    for t in &sections.followups {
        let _ = writeln!(o, "\n{}", sources::entry(t));
    }
    let _ = writeln!(o, "\n= New business =");
    for t in &sections.new_business {
        let _ = writeln!(o, "\n{}", sources::entry(t));
    }
    let _ = writeln!(
        o,
        "\n= Council happenings =\n\
         \n\
         = Open Floor =\n\
         \n\
         For more complete details, please visit each individual\n\
         issue.  The report of the agenda items can be found at\n\
         {}\n\
         \n\
         If you would like to add something to this agenda, you can\n\
         reply to this e-mail, file a new issue at\n\
         {}/{}/{}, e-mail me directly,\n\
         or bring it up at the end of the meeting, during the open floor\n\
         topic. Note that added topics may be deferred until the following\n\
         meeting.",
        sources::AGENDA_URL,
        sources::FORGE_URL,
        sources::TRACKER_OWNER,
        sources::TRACKER_REPO,
    );
    o
}

/// Ask how each unclear comment on a ticket should count.
///
/// The tool cannot tell "I'm +1 assuming X, but -1 if Y" from a
/// vote, so it shows what was written and lets the chair say. The
/// answer is the one `--vote N:FAS=V` would have given, applied
/// without a second run.
fn resolve_unclear_votes(report: &mut crate::votes::VoteReport) -> std::io::Result<()> {
    resolve_with(report, Utc::now(), |_, _| {
        use std::io::{BufRead, Write};
        loop {
            eprint!("  count it as? [+1/0/-1/Enter to leave unclear]: ");
            std::io::stderr().flush()?;
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            if let Some(how) = crate::votes::parse_unclear_answer(&line) {
                return Ok(how);
            }
            eprintln!("  answer +1, 0, -1, or Enter");
        }
    })
}

/// The loop itself, with answers coming from `ask`.
///
/// Split from the reading so it can be driven by canned answers: it
/// decides what is shown, in what order, and what each answer does
/// to the tally, none of which should need a terminal to exercise.
fn resolve_with<F>(
    report: &mut crate::votes::VoteReport,
    now: chrono::DateTime<Utc>,
    mut ask: F,
) -> std::io::Result<()>
where
    F: FnMut(&str, &crate::votes::Unclear) -> std::io::Result<Option<crate::votes::Vote>>,
{
    let unclear: Vec<(String, crate::votes::Unclear)> = report
        .unclear_comments
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (who, comment) in unclear {
        eprintln!(
            "  {who} wrote both a +1 and a -1:\n    {}",
            crate::votes::unclear_excerpt(&comment.body, 68)
        );
        if !comment.url.is_empty() {
            eprintln!("    {}", comment.url);
        }
        let how = ask(&who, &comment)?;
        crate::votes::resolve_unclear(report, &who, how, now);
        if let Some(how) = how {
            eprintln!(
                "    (same as {})",
                crate::votes::vote_flag(report.ticket.number, &who, how)
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::Ticket;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, 7).unwrap()
    }

    fn ticket(number: u64, title: &str) -> Ticket {
        Ticket {
            number,
            title: title.to_string(),
            url: format!("https://forge.fedoraproject.org/fesco/tickets/issues/{number}"),
            decision: None,
            repo: None,
            pull: false,
            created: None,
            updated: None,
        }
    }

    /// A report with one unclear vote, as #3696 carried.
    fn unclear_report() -> crate::votes::VoteReport {
        use crate::votes::{Unclear, classify, tally};
        let roster = ["ngompa", "salimma", "zbyszek", "decathorpe"]
            .map(str::to_string)
            .to_vec();
        let votes = tally(
            [
                ("ngompa", "+1"),
                ("salimma", "+1"),
                ("zbyszek", "+1"),
                ("decathorpe", "I'm +1 assuming X, but -1 if Y"),
            ]
            .into_iter(),
            &roster,
            &std::collections::BTreeMap::new(),
        );
        let start = "2026-10-18T12:00:00Z".parse().unwrap();
        let now: chrono::DateTime<Utc> = "2026-10-20T09:00:00Z".parse().unwrap();
        crate::votes::VoteReport {
            ticket: ticket(3696, "[FastTrack] gedit exception"),
            fast_track: true,
            vote_label: true,
            announced: None,
            unclear_comments: std::collections::BTreeMap::from([(
                "decathorpe".to_string(),
                Unclear {
                    body: "I'm +1 assuming X, but -1 if Y".to_string(),
                    url: "https://forge.example/3696#issuecomment-1367373".to_string(),
                },
            )]),
            start,
            start_basis: "ticket creation",
            tally: votes.counts(),
            verdict: classify(&votes, true, start, now),
            votes,
            reminder: None,
        }
    }

    fn now() -> chrono::DateTime<Utc> {
        "2026-10-20T09:00:00Z".parse().unwrap()
    }

    #[test]
    fn leaving_an_unclear_vote_alone_changes_nothing() {
        let mut report = unclear_report();
        let before = report.tally.clone();
        let mut asked = Vec::new();
        resolve_with(&mut report, now(), |who, c| {
            asked.push((who.to_string(), c.url.clone()));
            Ok(None)
        })
        .unwrap();
        // The member and the anchor are both offered to the caller,
        // which is what the prompt prints.
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, "decathorpe");
        assert!(asked[0].1.ends_with("#issuecomment-1367373"));
        assert_eq!(report.tally, before);
        assert_eq!(report.votes.unclear, vec!["decathorpe".to_string()]);
    }

    #[test]
    fn answering_an_unclear_vote_recounts_and_rejudges_it() {
        let mut report = unclear_report();
        assert_eq!(report.tally, "(+3, 0, -0)");
        assert!(report.verdict.detail.contains("unclear vote"));

        resolve_with(&mut report, now(), |_, _| {
            Ok(Some(crate::votes::Vote::Minus))
        })
        .unwrap();
        assert_eq!(report.tally, "(+3, 0, -1)");
        assert!(report.votes.unclear.is_empty());
        assert!(report.unclear_comments.is_empty());
        // Same destination, but now on a recorded objection.
        assert_eq!(report.verdict.outcome, crate::votes::Outcome::Meeting);
        assert!(
            report.verdict.detail.starts_with("-1 from decathorpe"),
            "{}",
            report.verdict.detail
        );
    }

    #[test]
    fn a_ticket_with_nothing_unclear_asks_nothing() {
        let mut report = unclear_report();
        report.unclear_comments.clear();
        report.votes.unclear.clear();
        let mut asked = 0;
        resolve_with(&mut report, now(), |_, _| {
            asked += 1;
            Ok(None)
        })
        .unwrap();
        assert_eq!(asked, 0);
    }

    #[test]
    fn a_failure_to_read_an_answer_stops_rather_than_guessing() {
        let mut report = unclear_report();
        let before = report.tally.clone();
        let err = resolve_with(&mut report, now(), |_, _| {
            Err(std::io::Error::other("stdin closed"))
        })
        .expect_err("a read failure is not an answer");
        assert_eq!(err.to_string(), "stdin closed");
        // Nothing was counted on the strength of a failed read.
        assert_eq!(report.tally, before);
    }

    #[test]
    fn render_body_docs_item_carries_repo_prefix() {
        let mut docs = ticket(28, "Clarify updates policy");
        docs.repo = Some("fesco/docs".to_string());
        docs.url = "https://forge.fedoraproject.org/fesco/docs/pulls/28".to_string();
        let sections = Sections {
            voted: vec![],
            followups: vec![],
            new_business: vec![docs],
        };
        let body = render_body(date(), &sections);
        assert!(
            body.contains(
                "fesco/docs#28 Clarify updates policy\n\
                 https://forge.fedoraproject.org/fesco/docs/pulls/28"
            ),
            "{body}"
        );
    }

    #[test]
    fn render_body_placeholder_without_parsed_decision() {
        let sections = Sections {
            voted: vec![ticket(3610, "T")],
            followups: vec![],
            new_business: vec![],
        };
        let body = render_body(date(), &sections);
        assert!(body.contains("DECISION (+X, Y, -Z)"), "{body}");
    }

    #[test]
    fn render_body_stays_within_the_wrap_width() {
        // Guards the static template as much as the entries: a line
        // over the width gets rewrapped by the sending mail client,
        // which orphans its tail. Only a line carrying a URL longer
        // than the width may exceed it, since splitting a URL stops it
        // being a link.
        let mut voted = ticket(3656, "");
        voted.title = "[FastTrack] Proposal: gate all stable release updates on \
                       rmdepcheck, everywhere"
            .to_string();
        voted.decision = Some("APPROVED (+8, 0, -0)".to_string());
        let sections = Sections {
            voted: vec![voted],
            followups: vec![ticket(
                3628,
                "Change: libxml215, with a deliberately overlong title to force a wrap",
            )],
            new_business: vec![ticket(
                3636,
                "Change: Enable Shadow Stack by Default on x86_64",
            )],
        };
        let body = render_body(date(), &sections);
        for line in body.lines() {
            if line.chars().count() <= sources::WRAP {
                continue;
            }
            let longest = line.split_whitespace().map(str::len).max().unwrap_or(0);
            assert!(
                longest > sources::WRAP,
                "line over {} without an unbreakable URL: {line:?}",
                sources::WRAP
            );
        }
    }

    #[test]
    fn an_announced_agenda_parses_back_to_the_sections_it_was_rendered_from() {
        // The round trip is the contract: `parse_agenda` has to read
        // whatever `render_body` writes, including a title long
        // enough to have been wrapped and an entry on another repo.
        let mut voted = ticket(3656, "Proposal: gate stable updates on rmdepcheck");
        voted.decision = Some("APPROVED (+8, 0, -0)".to_string());
        let mut docs = ticket(28, "Clarify the updates policy");
        docs.repo = Some("fesco/docs".to_string());
        docs.url = "https://forge.fedoraproject.org/fesco/docs/pulls/28".to_string();
        docs.pull = true;
        // An issue on the same repo, which the mail distinguishes
        // from the pull request only by the URL path -- and that
        // distinction is what picks `!forge issue` over `!forge pr`.
        let mut docs_issue = ticket(31, "Write down the quorum rule");
        docs_issue.repo = Some("fesco/docs".to_string());
        docs_issue.url = "https://forge.fedoraproject.org/fesco/docs/issues/31".to_string();
        let sections = Sections {
            voted: vec![voted],
            followups: vec![ticket(
                3628,
                "Change: libxml2 15, with a deliberately overlong title that forces a wrap",
            )],
            new_business: vec![
                ticket(3636, "Change: Enable Shadow Stack by Default"),
                docs,
                docs_issue,
            ],
        };
        let back = sources::parse_agenda(&render_body(date(), &sections));
        assert_eq!(back.voted, sections.voted);
        assert_eq!(back.followups, sections.followups);
        assert_eq!(back.new_business, sections.new_business);
    }

    #[test]
    fn an_agenda_carries_the_date_it_announces() {
        let sections = Sections {
            voted: vec![],
            followups: vec![],
            new_business: vec![],
        };
        let body = render_body(date(), &sections);
        assert_eq!(sources::agenda_date(&body), Some(date()));
    }

    #[test]
    fn an_empty_agenda_parses_to_no_tickets() {
        // The boilerplate alone: the preamble carries a URL and the
        // closing paragraphs carry two more, and none is an entry.
        let sections = Sections {
            voted: vec![],
            followups: vec![],
            new_business: vec![],
        };
        let back = sources::parse_agenda(&render_body(date(), &sections));
        assert!(back.voted.is_empty(), "{:?}", back.voted);
        assert!(back.followups.is_empty(), "{:?}", back.followups);
        assert!(back.new_business.is_empty(), "{:?}", back.new_business);
    }

    #[test]
    fn the_agenda_body_never_ends_a_line_in_whitespace() {
        // Every section populated, so the static template and the
        // wrapped entries are both covered; an empty section is
        // covered by the sibling test that omits one.
        let mut voted = ticket(3656, "Proposal: gate stable updates on rmdepcheck");
        voted.decision = Some("APPROVED (+8, 0, -0)".to_string());
        let sections = Sections {
            voted: vec![voted],
            followups: vec![ticket(
                3628,
                "Change: libxml2 15, with a deliberately overlong title to force a wrap",
            )],
            new_business: vec![ticket(3636, "Change: Enable Shadow Stack by Default")],
        };
        sources::assert_no_trailing_whitespace("agenda body", &render_body(date(), &sections));
    }

    #[test]
    fn subject_embeds_date() {
        assert_eq!(
            subject(date()),
            "Schedule for Tuesday's FESCo Meeting (2026-07-07)"
        );
    }

    #[test]
    fn render_body_full_template() {
        let mut voted = ticket(3610, "Grant provenpackager to X");
        voted.decision = Some("APPROVED (+6, 0, 0)".to_string());
        let sections = Sections {
            voted: vec![voted],
            followups: vec![ticket(3623, "Planning for the Forgejo distgit migration")],
            new_business: vec![ticket(3630, "F45 Change: Unified Kernel Images Phase 4")],
        };
        let body = render_body(date(), &sections);
        let expected = "\
Following is the list of topics that will be discussed in the FESCo
meeting Tuesday at 18:00 Europe/London in #meeting:fedoraproject.org
on Matrix.

To convert Europe/London (UTC/UTC+1) to your local time, take a look at
  https://fedoraproject.org/wiki/UTCHowto

or run:
  date -d 'TZ=\"Europe/London\" 2026-07-07 18:00'

Links to all issues to be discussed can be found at:
https://forge.fedoraproject.org/fesco/tickets/issues?labels=6114

= Discussed and Voted in the Ticket =

#3610 Grant provenpackager to X
https://forge.fedoraproject.org/fesco/tickets/issues/3610
APPROVED (+6, 0, 0)

= Followups =

#3623 Planning for the Forgejo distgit migration
https://forge.fedoraproject.org/fesco/tickets/issues/3623

= New business =

#3630 F45 Change: Unified Kernel Images Phase 4
https://forge.fedoraproject.org/fesco/tickets/issues/3630

= Council happenings =

= Open Floor =

For more complete details, please visit each individual
issue.  The report of the agenda items can be found at
https://forge.fedoraproject.org/fesco/tickets/issues?labels=6114

If you would like to add something to this agenda, you can
reply to this e-mail, file a new issue at
https://forge.fedoraproject.org/fesco/tickets, e-mail me directly,
or bring it up at the end of the meeting, during the open floor
topic. Note that added topics may be deferred until the following
meeting.
";
        assert_eq!(body, expected);
    }

    #[test]
    fn render_body_omits_empty_voted_section() {
        let sections = Sections {
            voted: vec![],
            followups: vec![],
            new_business: vec![ticket(3630, "T")],
        };
        let body = render_body(date(), &sections);
        assert!(!body.contains("Discussed and Voted"), "{body}");
        // Followups stays even when empty, for manual additions.
        assert!(body.contains("= Followups ="), "{body}");
    }
}
