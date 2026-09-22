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
            "\nreminder: comment \"This issue will be discussed at the next \
             meeting on {date}\" on each meeting ticket (see the wiki's \
             pre-meeting list)\n\
             after sending: on each announced ticket, comment \
             \"Announced: <archive link>\", untag `pending announcement`, \
             and close it with the matching status"
        );
        if !votes_taken.is_empty() {
            let list: Vec<String> = votes_taken.iter().map(|n| format!("#{n}")).collect();
            eprintln!(
                "tag `meeting` on {}: taken onto the agenda from an in-ticket \
                 vote, so the tracker still shows only the vote label",
                list.join(", ")
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
    let voted = sources::pending_tickets(&client)?;
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
    for report in crate::votes::open_votes(&client, &args.voters, args.verbose, Utc::now()) {
        let number = report.ticket.number;
        if voted.iter().chain(&meeting).any(|t| t.number == number) {
            continue;
        }
        let take = interactive && {
            eprintln!(
                "\n{}\n  {}",
                report.ticket.url,
                crate::votes::brief(&report, date)
            );
            sandogasa_cli::confirm(
                &format!(
                    "add {} \u{201c}{}\u{201d} to the agenda?",
                    report.ticket.label(),
                    report.ticket.title
                ),
                report.verdict.outcome == crate::votes::Outcome::Meeting,
            )?
        };
        if take {
            votes_taken.push(number);
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
                        "\n{}\n  opened {}, last updated {}",
                        item.url,
                        item.created.as_deref().unwrap_or("?"),
                        item.updated.as_deref().unwrap_or("?")
                    );
                }
                let take = interactive
                    && sources::confirm_default_no(&format!(
                        "add {} \u{201c}{}\u{201d} to the agenda?",
                        item.label(),
                        item.title
                    ))?;
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
