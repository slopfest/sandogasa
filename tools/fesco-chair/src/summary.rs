// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `summary` subcommand — compose the post-meeting summary email from
//! the meetbot minutes, sent as a reply to the schedule announcement.

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
    let body = render_body(
        &meeting.summary_url,
        &minutes_txt_url,
        &meeting.logs_url,
        &log_txt_url,
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
            body,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    } else {
        print!("Subject: {}\n\n{body}", subject(date));
        eprintln!(
            "\nreminder: send this as a reply to the schedule announcement \
             (same thread), then comment/close the discussed tickets"
        );
    }
    ExitCode::SUCCESS
}

/// The summary subject line.
pub fn subject(date: NaiveDate) -> String {
    format!("Summary/Minutes from today's FESCo Meeting ({date})")
}

/// The email body: the artefact links, then the full plain-text
/// minutes.
pub fn render_body(
    minutes_url: &str,
    minutes_txt_url: &str,
    log_url: &str,
    log_txt_url: &str,
    minutes: &str,
) -> String {
    let mut o = format!(
        "Minutes: {minutes_url}\n\
         Minutes (text): {minutes_txt_url}\n\
         Log: {log_url}\n\
         Log (text): {log_txt_url}\n\
         \n\
         {minutes}"
    );
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
    fn render_body_links_then_minutes() {
        let body = render_body(
            "https://m/f.html",
            "https://m/f.txt",
            "https://m/f.log.html",
            "https://m/f.log.txt",
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
}
