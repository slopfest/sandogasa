// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `summary` subcommand — compose the post-meeting summary email from
//! the meetbot minutes, sent as a reply to the schedule announcement.
//!
//! Tickets still tagged `pending announcement` are announced here
//! too: the label is dropped once an announcement goes out, so
//! anything still carrying it was decided in-ticket after the
//! schedule was sent and would otherwise never be announced.

use std::process::ExitCode;

use chrono::NaiveDate;

use crate::sources;

#[derive(clap::Args)]
pub struct SummaryArgs {
    /// Meeting date (default: today).
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub date: Option<NaiveDate>,

    /// Machine-readable JSON output.
    #[arg(long)]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(serde::Serialize)]
struct SummaryJson {
    date: String,
    subject: String,
    minutes_url: String,
    minutes_txt_url: String,
    log_url: String,
    log_txt_url: String,
    /// Tickets announced alongside the minutes (still tagged
    /// `pending announcement` when this ran).
    voted: Vec<sources::Ticket>,
    body: String,
}

pub fn run(args: &SummaryArgs) -> ExitCode {
    let date = args
        .date
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let meetbot = sandogasa_meetbot::Meetbot::new();
    let meeting = match sources::find_meeting(&meetbot, date) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let minutes_txt_url = sources::txt_url(&meeting.summary_url);
    let log_txt_url = sources::txt_url(&meeting.logs_url);
    if args.verbose {
        eprintln!("[summary] fetching {minutes_txt_url}");
    }
    let minutes = match sources::fetch_text(&sources::http_client(), &minutes_txt_url) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let voted = pending_announcements(args.verbose);
    let body = render_body(
        &meeting.summary_url,
        &minutes_txt_url,
        &meeting.logs_url,
        &log_txt_url,
        &voted,
        &minutes,
    );
    if args.json {
        let out = SummaryJson {
            date: date.to_string(),
            subject: subject(date),
            minutes_url: meeting.summary_url,
            minutes_txt_url,
            log_url: meeting.logs_url,
            log_txt_url,
            voted,
            body,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    } else {
        print!("Subject: {}\n\n{body}", subject(date));
        eprintln!(
            "\nreminder: send this as a reply to the schedule announcement \
             (same thread), then comment/close the discussed tickets"
        );
        if !voted.is_empty() {
            eprintln!(
                "after sending: on each of the {} announced ticket(s) above, \
                 comment \"Announced: <archive link>\", untag `pending \
                 announcement`, and close it with the matching status",
                voted.len()
            );
        }
    }
    // The meeting is over; the saved agenda has served its purpose.
    if crate::state::clear() && args.verbose {
        eprintln!("[summary] cleared the saved agenda state");
    }
    ExitCode::SUCCESS
}

/// The summary subject line.
pub fn subject(date: NaiveDate) -> String {
    format!("Summary/Minutes from today's FESCo Meeting ({date})")
}

/// Column the email body is wrapped to. meetbot's minutes routinely
/// run past 200 columns, so something has to wrap them — and if we
/// don't, the sending mail client does, at its own width and with
/// continuations flush left, which loses the bullet structure.
///
/// The width must not exceed the client's, or every line is broken a
/// second time and the result is littered with one- and two-word
/// orphans — which is what 80 produced in the 2026-07-28 summary.
///
/// 71 rather than the conventional 72
/// (<https://useplaintext.email/#etiquette>): that summary's archived
/// copy contains surviving 71-column lines, so 71 is known to pass
/// through untouched, while nothing reaches 72 anywhere in it despite
/// lines clustering at 66-71 — so 72 may well be one past the break.
/// Both are within the convention; this is the one with evidence.
const WRAP: usize = 71;

/// Word-wrap one minutes line, keeping its leading indent and `*`
/// bullet on the first line and aligning continuations under the
/// text. Lines already within [`WRAP`] — blanks, separators, the
/// attendee tallies — pass through untouched, and a word longer than
/// the width (a bare URL) overflows rather than being broken, since a
/// split URL stops being a link.
fn wrap_line(line: &str) -> String {
    if line.chars().count() <= WRAP {
        return line.to_string();
    }
    let indent = line.len() - line.trim_start().len();
    let bullet = if line[indent..].starts_with("* ") {
        2
    } else {
        0
    };
    let (prefix, content) = line.split_at(indent + bullet);
    // Continuations align under the text; same width as the prefix, so
    // swapping it back in below keeps every line's length correct.
    let hang = " ".repeat(prefix.len());
    let wrapped = sandogasa_cli::wrap_prefixed(content, &hang, WRAP);
    format!("{prefix}{}", &wrapped[hang.len()..])
}

/// Tickets to announce alongside the minutes, with their decisions.
/// Best-effort: without a token or a reachable tracker the summary
/// still stands on the minutes alone, so any failure warns rather
/// than aborting a run whose expensive part already succeeded.
fn pending_announcements(verbose: bool) -> Vec<sources::Ticket> {
    let client = match sources::forge_client() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("warning: not checking for pending announcements ({e})");
            return Vec::new();
        }
    };
    if verbose {
        eprintln!("[summary] fetching '{}' tickets", sources::PENDING_LABEL);
    }
    let mut voted = match sources::pending_tickets(&client) {
        Ok(voted) => voted,
        Err(e) => {
            eprintln!("warning: could not fetch pending announcements ({e})");
            return Vec::new();
        }
    };
    sources::fill_decisions(&client, &mut voted, verbose);
    voted
}

/// The email body: the artefact links, any tickets decided in-ticket
/// but not yet announced, then the full plain-text minutes. The
/// section header matches the schedule announcement's, since it lists
/// the same kind of item; it is omitted when there is nothing to
/// announce.
///
/// Ticket titles and the minutes are wrapped to [`WRAP`] columns. The
/// artefact links aren't: each is one unbreakable URL already past
/// the width, and wrapping would only orphan its label.
pub fn render_body(
    minutes_url: &str,
    minutes_txt_url: &str,
    log_url: &str,
    log_txt_url: &str,
    voted: &[sources::Ticket],
    minutes: &str,
) -> String {
    use std::fmt::Write as _;
    let mut o = format!(
        "Minutes: {minutes_url}\n\
         Minutes (text): {minutes_txt_url}\n\
         Log: {log_url}\n\
         Log (text): {log_txt_url}\n"
    );
    if !voted.is_empty() {
        let _ = writeln!(o, "\n= Discussed and Voted in the Ticket =");
        for t in voted {
            let decision = t.decision.as_deref().unwrap_or("DECISION (+X, Y, -Z)");
            let title = wrap_line(&format!("{} {}", t.label(), t.title));
            let _ = writeln!(o, "\n{title}\n{}\n{decision}", t.url);
        }
    }
    o.push('\n');
    for line in minutes.lines() {
        let _ = writeln!(o, "{}", wrap_line(line));
    }
    if !o.ends_with('\n') {
        o.push('\n');
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_embeds_date() {
        assert_eq!(
            subject(NaiveDate::from_ymd_opt(2026, 7, 7).unwrap()),
            "Summary/Minutes from today's FESCo Meeting (2026-07-07)"
        );
    }

    #[test]
    fn wrap_line_keeps_bullets_and_aligns_continuations() {
        // A nested AGREED line from the 2026-07-21 minutes: the bullet
        // stays put and continuations align under the text.
        let line = "    * AGREED: FESCo asks the Change Owner to update the wiki page to \
                    ensure critical path packages are fixed before the change is merged \
                    (+6, 0, 0) (@zbyszek:fedora.im, 17:25:10)";
        let wrapped = wrap_line(line);
        let mut lines = wrapped.lines();
        assert!(lines.next().unwrap().starts_with("    * AGREED: FESCo"));
        for cont in lines {
            assert!(cont.starts_with("      "), "not aligned: {cont:?}");
            assert!(
                !cont.trim_start().starts_with('*'),
                "false bullet: {cont:?}"
            );
        }
        assert!(
            wrapped.lines().all(|l| l.chars().count() <= WRAP),
            "{wrapped}"
        );
        // No words lost or duplicated.
        assert_eq!(
            wrapped.split_whitespace().collect::<Vec<_>>(),
            line.split_whitespace().collect::<Vec<_>>()
        );
        // Short lines, blanks and separators pass through untouched.
        for short in [
            "",
            "---------------",
            "* TOPIC: Init Process (@z, 17:01:36)",
        ] {
            assert_eq!(wrap_line(short), short);
        }
    }

    #[test]
    fn render_body_links_then_minutes() {
        let body = render_body(
            "https://m/f.html",
            "https://m/f.txt",
            "https://m/f.log.html",
            "https://m/f.log.txt",
            &[],
            "Meeting summary\n---------------\n* TOPIC: Init Process\n",
        );
        let expected = "\
Minutes: https://m/f.html
Minutes (text): https://m/f.txt
Log: https://m/f.log.html
Log (text): https://m/f.log.txt

Meeting summary
---------------
* TOPIC: Init Process
";
        assert_eq!(body, expected);
    }

    #[test]
    fn render_body_announces_pending_tickets() {
        // A ticket tagged after the schedule went out (the real
        // ticket-3656 shape), announced with the minutes.
        let voted = vec![
            sources::Ticket {
                number: 3656,
                title: "[FastTrack] Proposal: gate stable updates on rmdepcheck".to_string(),
                url: "https://forge.fedoraproject.org/fesco/tickets/issues/3656".to_string(),
                decision: Some("APPROVED (+8, 0, -0)".to_string()),
                repo: None,
                pull: false,
            },
            // No tally parsed → the chair fills the line in.
            sources::Ticket {
                number: 3660,
                title: "T".to_string(),
                url: "https://forge.fedoraproject.org/fesco/tickets/issues/3660".to_string(),
                decision: None,
                repo: None,
                pull: false,
            },
        ];
        let body = render_body(
            "https://m/f.html",
            "https://m/f.txt",
            "https://m/f.log.html",
            "https://m/f.log.txt",
            &voted,
            "Meeting summary\n---------------\n* TOPIC: Init Process\n",
        );
        let expected = "\
Minutes: https://m/f.html
Minutes (text): https://m/f.txt
Log: https://m/f.log.html
Log (text): https://m/f.log.txt

= Discussed and Voted in the Ticket =

#3656 [FastTrack] Proposal: gate stable updates on rmdepcheck
https://forge.fedoraproject.org/fesco/tickets/issues/3656
APPROVED (+8, 0, -0)

#3660 T
https://forge.fedoraproject.org/fesco/tickets/issues/3660
DECISION (+X, Y, -Z)

Meeting summary
---------------
* TOPIC: Init Process
";
        assert_eq!(body, expected);
    }
}
