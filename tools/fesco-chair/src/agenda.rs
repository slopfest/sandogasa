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

use chrono::NaiveDate;

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
    body: String,
}

pub fn run(args: &AgendaArgs) -> ExitCode {
    let (date, sections, docs_open) = match assemble(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Persist the assembled agenda so `script` can replay these
    // decisions on meeting day without re-asking; `summary` clears it.
    let state = crate::state::AgendaState {
        date,
        sections,
        docs_open,
    };
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

/// Fetch the ticket pools and split them into sections; also returns
/// the open fesco/docs items *not* put on the agenda (surfaced so the
/// chair can reconsider). Shared with the `script` subcommand, which
/// runs the same classification.
pub fn assemble(
    args: &AgendaArgs,
) -> Result<(NaiveDate, Sections, Vec<sources::Ticket>), Box<dyn std::error::Error>> {
    let date = target_date(args);

    let client = sources::forge_client()?;
    if args.verbose {
        eprintln!("[agenda] fetching '{}' tickets", sources::PENDING_LABEL);
    }
    // Only *open* pending-announcement tickets: the flow is tag →
    // announce → untag + close, so a closed ticket still carrying the
    // label is stale state from a past announcement, not agenda
    // material.
    let voted: Vec<sources::Ticket> = client
        .repo_issues(
            sources::TRACKER_OWNER,
            sources::TRACKER_REPO,
            "open",
            &[sources::PENDING_LABEL],
        )?
        .into_iter()
        .map(Into::into)
        .collect();
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
            let interactive = !args.json && std::io::IsTerminal::is_terminal(&std::io::stdin());
            for item in rest {
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

    // Parse each announced ticket's decision from its comments (the
    // vote concludes with e.g. "After a week: APPROVED (+3, 0, 0)"
    // right before the ticket is tagged). Best-effort: a fetch or
    // parse miss leaves the template placeholder for the chair.
    for ticket in &mut sections.voted {
        if args.verbose {
            eprintln!("[agenda] parsing decision for #{}", ticket.number);
        }
        match client.issue_comments(sources::TRACKER_OWNER, sources::TRACKER_REPO, ticket.number) {
            Ok(comments) => {
                ticket.decision =
                    sources::extract_decision(comments.iter().rev().map(|c| c.body.as_str()));
                if ticket.decision.is_none() {
                    eprintln!(
                        "warning: no APPROVED/REJECTED tally found in #{}'s comments; \
                         fill in the DECISION line by hand",
                        ticket.number
                    );
                }
            }
            Err(e) => eprintln!(
                "warning: could not fetch #{}'s comments ({e}); fill in the \
                 DECISION line by hand",
                ticket.number
            ),
        }
    }

    Ok((date, sections, docs_open))
}

/// Render the announcement body (everything below the Subject line),
/// following the wiki template. The "Discussed and Voted in the
/// Ticket" section is omitted when empty (matching the wiki's
/// sample); Followups and New business always appear so the chair
/// can slot in late additions.
pub fn render_body(date: NaiveDate, sections: &Sections) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "Following is the list of topics that will be discussed in the\n\
         FESCo meeting Tuesday at 18:00 Europe/London in #meeting:fedoraproject.org\n\
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
            let _ = writeln!(o, "\n{} {}\n{}\n{decision}", t.label(), t.title, t.url);
        }
    }
    let _ = writeln!(o, "\n= Followups =");
    for t in &sections.followups {
        let _ = writeln!(o, "\n{} {}\n{}", t.label(), t.title, t.url);
    }
    let _ = writeln!(o, "\n= New business =");
    for t in &sections.new_business {
        let _ = writeln!(o, "\n{} {}\n{}", t.label(), t.title, t.url);
    }
    let _ = writeln!(
        o,
        "\n= Open Floor =\n\
         \n\
         For more complete details, please visit each individual\n\
         issue.  The report of the agenda items can be found at\n\
         {}\n\
         \n\
         If you would like to add something to this agenda, you can\n\
         reply to this e-mail, file a new issue at\n\
         {}/{}/{}, e-mail me directly, or bring it\n\
         up at the end of the meeting, during the open floor topic. Note\n\
         that added topics may be deferred until the following meeting.",
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
Following is the list of topics that will be discussed in the
FESCo meeting Tuesday at 18:00 Europe/London in #meeting:fedoraproject.org
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

= Open Floor =

For more complete details, please visit each individual
issue.  The report of the agenda items can be found at
https://forge.fedoraproject.org/fesco/tickets/issues?labels=6114

If you would like to add something to this agenda, you can
reply to this e-mail, file a new issue at
https://forge.fedoraproject.org/fesco/tickets, e-mail me directly, or bring it
up at the end of the meeting, during the open floor topic. Note
that added topics may be deferred until the following meeting.
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
