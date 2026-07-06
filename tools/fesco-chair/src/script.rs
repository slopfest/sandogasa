// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `script` subcommand — the day-of checklist and the meetbot command
//! script the chair copy/pastes from as the meeting progresses.

use std::process::ExitCode;

use chrono::NaiveDate;

use crate::agenda::AgendaArgs;
use crate::sources::Ticket;

/// The spam-filter-safe reminder message for #devel:fedoraproject.org.
pub const REMINDER_COMMAND: &str = "!group members fesco";

#[derive(clap::Args)]
pub struct ScriptArgs {
    // The script is generated from the same ticket classification as
    // the agenda, so it takes the same knobs.
    #[command(flatten)]
    pub agenda: AgendaArgs,
}

#[derive(serde::Serialize)]
struct ScriptJson<'a> {
    date: String,
    reminder: &'static str,
    tickets: Vec<&'a Ticket>,
    script: String,
}

pub fn run(args: &ScriptArgs) -> ExitCode {
    // Replay the agenda saved by `fesco-chair agenda` when it matches
    // this meeting date — no refetching, no re-asking about docs
    // items. Any override flag signals a re-decide, so it reassembles
    // (and re-saves) instead.
    let date = crate::agenda::target_date(&args.agenda);
    let sections = match crate::state::load(date) {
        Some(state) if !crate::agenda::has_overrides(&args.agenda) => {
            if let Some(path) = crate::state::state_path() {
                eprintln!(
                    "using the agenda saved for {date} ({}); pass an override \
                     flag or delete the file to regenerate",
                    path.display()
                );
            }
            state.sections
        }
        _ => match crate::agenda::assemble(&args.agenda) {
            Ok((date, sections, docs_open)) => {
                let state = crate::state::AgendaState {
                    date,
                    sections,
                    docs_open,
                };
                crate::state::save(&state);
                state.sections
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let discussion = sections.discussion();
    let script = render_script(date, &discussion);
    if args.agenda.json {
        let out = ScriptJson {
            date: date.to_string(),
            reminder: REMINDER_COMMAND,
            tickets: discussion,
            script,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    } else {
        // Checklist on stderr so stdout stays a clean, pipeable
        // script (`fesco-chair script > meeting.txt`).
        eprint!("{}", checklist(date));
        print!("{script}");
    }
    ExitCode::SUCCESS
}

/// The day-of prompts from the wiki, ahead of the script itself.
pub fn checklist(date: NaiveDate) -> String {
    format!(
        "── day of meeting ({date}) ──\n\
         1. A bit before the meeting, remind #devel:fedoraproject.org.\n\
         \x20  The spam-filter-safe way is to send exactly:  {REMINDER_COMMAND}\n\
         2. In #meeting:fedoraproject.org, paste the script lines below\n\
         \x20  (stdout) as the meeting progresses.\n\
         3. After Init Process, wait for quorum: at least 5 voting\n\
         \x20  members present, else cancel the meeting.\n\
         4. Watch the clock per topic: at 15 minutes, vote to extend by\n\
         \x20  another 15, or ask for a written proposal on the wiki for\n\
         \x20  next meeting.\n\
         ────\n"
    )
}

/// The meetbot command script, per the wiki template: followups
/// first, then new business, each with a ticket info lookup and an
/// `!agreed` placeholder recording the vote tally.
///
/// The lookup is `!forge issue fesco tickets NNNN` — the `!fesco NNNN`
/// alias is currently broken; switch back once
/// <https://github.com/fedora-infra/maubot-fedora/pull/154> is merged
/// and deployed (tracked in TODO.md).
pub fn render_script(date: NaiveDate, discussion: &[&Ticket]) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "!startmeeting FESCO ({date})\n\
         !meetingname fesco\n\
         !group members fesco\n\
         !topic Init Process"
    );
    for t in discussion {
        // The maubot lookup takes owner + repo, so docs items work
        // too (`!forge issue fesco docs NNNN`); pull requests have
        // their own subcommand (`!forge pr`).
        let kind = if t.pull { "pr" } else { "issue" };
        let repo_args = match &t.repo {
            Some(slug) => slug.replace('/', " "),
            None => "fesco tickets".to_string(),
        };
        let _ = writeln!(
            o,
            "!topic {} {}\n\
             !forge {kind} {repo_args} {}\n\
             !agreed DECISION (+X, Y, -Z)",
            t.label(),
            t.title,
            t.number
        );
    }
    let _ = writeln!(
        o,
        "!topic Next week's chair\n\
         !action NAME will chair next meeting\n\
         !topic Open Floor\n\
         !endmeeting"
    );
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_script_full_template() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
        let followup = Ticket {
            number: 3623,
            title: "Planning for the Forgejo distgit migration".to_string(),
            url: String::new(),
            decision: None,
            repo: None,
            pull: false,
        };
        let script = render_script(date, &[&followup]);
        let expected = "\
!startmeeting FESCO (2026-07-07)
!meetingname fesco
!group members fesco
!topic Init Process
!topic #3623 Planning for the Forgejo distgit migration
!forge issue fesco tickets 3623
!agreed DECISION (+X, Y, -Z)
!topic Next week's chair
!action NAME will chair next meeting
!topic Open Floor
!endmeeting
";
        assert_eq!(script, expected);
    }

    #[test]
    fn render_script_docs_item_looks_up_docs_repo() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
        let docs = Ticket {
            number: 28,
            title: "Clarify updates policy".to_string(),
            url: "https://forge.fedoraproject.org/fesco/docs/pulls/28".to_string(),
            decision: None,
            repo: Some("fesco/docs".to_string()),
            pull: true,
        };
        let script = render_script(date, &[&docs]);
        // A docs PR uses the pr subcommand against its own repo.
        assert!(
            script.contains(
                "!topic fesco/docs#28 Clarify updates policy\n\
                 !forge pr fesco docs 28\n\
                 !agreed DECISION (+X, Y, -Z)"
            ),
            "{script}"
        );
        assert!(!script.contains("!forge issue"), "{script}");

        // A docs *issue* keeps the issue subcommand.
        let mut issue = docs.clone();
        issue.pull = false;
        let script = render_script(date, &[&issue]);
        assert!(script.contains("!forge issue fesco docs 28"), "{script}");
    }

    #[test]
    fn checklist_mentions_reminder_and_quorum() {
        let text = checklist(NaiveDate::from_ymd_opt(2026, 7, 7).unwrap());
        assert!(text.contains(REMINDER_COMMAND), "{text}");
        assert!(text.contains("at least 5 voting"), "{text}");
        assert!(text.contains("15 minutes"), "{text}");
    }
}
