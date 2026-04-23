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
    let range = match resolve_date_range(args.since, args.until, args.period.as_deref()) {
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

/// Resolve an optional `--since` + `--until` pair, or a
/// `--period` shortcut, into an inclusive `(start, end)` range.
/// When nothing is supplied the range is unbounded
/// (`(NaiveDate::MIN, NaiveDate::MAX)`).
///
/// Mirrors the period-parsing shape used by `sandogasa-report`
/// — should be extracted to `sandogasa-cli` once a third
/// caller materializes.
pub(crate) fn resolve_date_range(
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
    period: Option<&str>,
) -> Result<(NaiveDate, NaiveDate), String> {
    if let Some(p) = period {
        return parse_period(p);
    }
    let Some(since) = since else {
        return Ok((NaiveDate::MIN, NaiveDate::MAX));
    };
    let until = until.unwrap_or_else(|| chrono::Local::now().date_naive());
    if since > until {
        return Err(format!("--since ({since}) is after --until ({until})"));
    }
    Ok((since, until))
}

/// Parse a period string like `2026`, `2026Q1`, or `2026H1`
/// into a `(start, end)` inclusive date range.
pub(crate) fn parse_period(period: &str) -> Result<(NaiveDate, NaiveDate), String> {
    let period = period.trim();
    if period.len() < 4 {
        return Err(format!(
            "invalid period: {period} (expected e.g. 2026, 2026Q1, or 2026H1)"
        ));
    }
    let (year_str, kind) = period.split_at(4);
    let year: i32 = year_str
        .parse()
        .map_err(|_| format!("invalid year in period: {period}"))?;
    if kind.is_empty() {
        return Ok((
            NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
        ));
    }
    match kind.to_uppercase().as_str() {
        "Q1" => Ok((
            NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 3, 31).unwrap(),
        )),
        "Q2" => Ok((
            NaiveDate::from_ymd_opt(year, 4, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 6, 30).unwrap(),
        )),
        "Q3" => Ok((
            NaiveDate::from_ymd_opt(year, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 9, 30).unwrap(),
        )),
        "Q4" => Ok((
            NaiveDate::from_ymd_opt(year, 10, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
        )),
        "H1" => Ok((
            NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 6, 30).unwrap(),
        )),
        "H2" => Ok((
            NaiveDate::from_ymd_opt(year, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
        )),
        _ => Err(format!(
            "invalid period: {period} (expected Q1-Q4 or H1-H2)"
        )),
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
    fn parse_period_bare_year() {
        let (s, e) = parse_period("2026").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
    }

    #[test]
    fn parse_period_quarters_and_halves() {
        let (s, e) = parse_period("2026Q2").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 4, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 6, 30).unwrap());
        let (s, e) = parse_period("2026H1").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 6, 30).unwrap());
        let (s, e) = parse_period("2026h2").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
    }

    #[test]
    fn parse_period_rejects_garbage() {
        assert!(parse_period("202").is_err());
        assert!(parse_period("abcd").is_err());
        assert!(parse_period("2026Q9").is_err());
    }

    #[test]
    fn resolve_date_range_defaults_to_open() {
        let (s, e) = resolve_date_range(None, None, None).unwrap();
        assert_eq!(s, NaiveDate::MIN);
        assert_eq!(e, NaiveDate::MAX);
    }

    #[test]
    fn resolve_date_range_prefers_period() {
        let (s, e) = resolve_date_range(None, None, Some("2026Q1")).unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
    }

    #[test]
    fn resolve_date_range_rejects_inverted_range() {
        let err = resolve_date_range(
            NaiveDate::from_ymd_opt(2026, 6, 1),
            NaiveDate::from_ymd_opt(2026, 1, 1),
            None,
        )
        .unwrap_err();
        assert!(err.contains("is after"));
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
