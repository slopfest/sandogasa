// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `list` subcommand — fetch meetings from meetbot and print.

use std::process::ExitCode;

use chrono::NaiveDate;
use sandogasa_meetbot::{Meetbot, Meeting};

/// Default topic searched. Hyperscale SIG meetings start with
/// `!startmeeting CentOS Hyperscale SIG`, which zodbot records
/// under the `centos-hyperscale-sig` slug.
const DEFAULT_TOPIC: &str = "centos-hyperscale-sig";

#[derive(clap::Args)]
pub struct ListArgs {
    /// Meetbot search topic (matches meeting topic substrings).
    #[arg(short, long, default_value = DEFAULT_TOPIC)]
    pub topic: String,

    /// Start date filter (inclusive, YYYY-MM-DD).
    #[arg(long, group = "date_range")]
    pub since: Option<NaiveDate>,

    /// End date filter (inclusive, YYYY-MM-DD, default: today).
    #[arg(long, requires = "since")]
    pub until: Option<NaiveDate>,

    /// Calendar period filter — e.g. `2026`, `2026Q1`, `2026H1`.
    #[arg(long, group = "date_range")]
    pub period: Option<String>,

    /// Emit JSON instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(serde::Serialize)]
struct Row<'a> {
    date: String,
    topic: &'a str,
    summary_url: &'a str,
    logs_url: &'a str,
}

pub fn run(args: &ListArgs) -> ExitCode {
    let range = match sandogasa_cli::date::resolve_date_range(
        args.since,
        args.until,
        args.period.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let client = Meetbot::new();
    let meetings = match client.search(&args.topic) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let meetings: Vec<Meeting> = meetings
        .into_iter()
        .filter(|m| in_range(m, range))
        .collect();

    if args.json {
        match serde_json::to_string_pretty(&to_rows(&meetings)) {
            Ok(j) => println!("{j}"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print_table(&meetings);
    }
    ExitCode::SUCCESS
}

fn to_rows(meetings: &[Meeting]) -> Vec<Row<'_>> {
    meetings
        .iter()
        .map(|m| Row {
            date: m.datetime.format("%Y-%m-%d %H:%M").to_string(),
            topic: &m.topic,
            summary_url: &m.summary_url,
            logs_url: &m.logs_url,
        })
        .collect()
}

fn print_table(meetings: &[Meeting]) {
    if meetings.is_empty() {
        println!("no meetings matched");
        return;
    }
    const H_DATE: &str = "DATE";
    const H_TOPIC: &str = "TOPIC";
    const H_SUMMARY: &str = "SUMMARY";
    let date_width = meetings
        .iter()
        .map(|m| m.datetime.format("%Y-%m-%d %H:%M").to_string().len())
        .max()
        .unwrap_or(0)
        .max(H_DATE.len());
    let topic_width = meetings
        .iter()
        .map(|m| m.topic.chars().count())
        .max()
        .unwrap_or(0)
        .max(H_TOPIC.len());
    println!(
        "{:<date_width$}  {:<topic_width$}  {}",
        H_DATE, H_TOPIC, H_SUMMARY,
    );
    for m in meetings {
        let date = m.datetime.format("%Y-%m-%d %H:%M").to_string();
        println!(
            "{:<date_width$}  {:<topic_width$}  {}",
            date, m.topic, m.summary_url,
        );
    }
}

fn in_range(meeting: &Meeting, range: (NaiveDate, NaiveDate)) -> bool {
    let d = meeting.datetime.date();
    d >= range.0 && d <= range.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meeting(ts: &str) -> Meeting {
        Meeting {
            channel: "c".to_string(),
            datetime: chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S").unwrap(),
            topic: "centos-hyperscale-sig".to_string(),
            summary_url: format!("https://example.org/s/{ts}"),
            logs_url: format!("https://example.org/l/{ts}"),
        }
    }

    #[test]
    fn to_rows_formats_date() {
        let meetings = vec![sample_meeting("2026-04-22T15:08:00")];
        let rows = to_rows(&meetings);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].date, "2026-04-22 15:08");
        assert_eq!(rows[0].topic, "centos-hyperscale-sig");
    }

    #[test]
    fn to_rows_empty_input() {
        assert!(to_rows(&[]).is_empty());
    }

    #[test]
    fn in_range_filters_outside() {
        let m = sample_meeting("2026-04-22T15:08:00");
        assert!(in_range(
            &m,
            (
                NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
            ),
        ));
        assert!(!in_range(
            &m,
            (
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            ),
        ));
    }
}
