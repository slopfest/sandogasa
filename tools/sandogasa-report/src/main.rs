// SPDX-License-Identifier: MPL-2.0

use std::process::ExitCode;

use chrono::NaiveDate;
use clap::Parser;

mod brace;
mod config;

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    before_help = concat!(
        env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")
    )
)]
struct Cli {
    /// FAS username to report on.
    #[arg(short, long)]
    user: Option<String>,

    /// Domain to report on (defined in config).
    #[arg(short, long)]
    domain: String,

    /// Start date (inclusive, YYYY-MM-DD).
    #[arg(long, group = "date_range")]
    since: Option<NaiveDate>,

    /// End date (inclusive, YYYY-MM-DD, default: today).
    #[arg(long, requires = "since")]
    until: Option<NaiveDate>,

    /// Reporting period (e.g. 2026Q1, 2026H1).
    #[arg(long, group = "date_range")]
    period: Option<String>,

    /// Include per-item details, not just counts.
    #[arg(long)]
    detailed: bool,

    /// Output as JSON instead of Markdown.
    #[arg(long)]
    json: bool,

    /// Write output to file instead of stdout.
    #[arg(short, long, value_name = "PATH")]
    output: Option<String>,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

/// Parse a period string like "2026Q1" or "2026H1" into a date range.
fn parse_period(period: &str) -> Result<(NaiveDate, NaiveDate), String> {
    let period = period.trim();
    if period.len() < 5 {
        return Err(format!(
            "invalid period: {period} (expected e.g. 2026Q1 or 2026H1)"
        ));
    }

    let (year_str, kind) = period.split_at(4);
    let year: i32 = year_str
        .parse()
        .map_err(|_| format!("invalid year in period: {period}"))?;

    let kind_upper = kind.to_uppercase();
    match kind_upper.as_str() {
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

/// Resolve the date range from CLI args.
fn resolve_date_range(cli: &Cli) -> Result<(NaiveDate, NaiveDate), String> {
    if let Some(ref period) = cli.period {
        return parse_period(period);
    }

    let since = cli.since.ok_or("either --since or --period is required")?;
    let until = cli
        .until
        .unwrap_or_else(|| chrono::Local::now().date_naive());

    if since > until {
        return Err(format!("--since ({since}) is after --until ({until})"));
    }

    Ok((since, until))
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let (since, until) = match resolve_date_range(&cli) {
        Ok(range) => range,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let cfg = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let domain = match cfg.domains.get(&cli.domain) {
        Some(d) => d,
        None => {
            let available: Vec<&str> = cfg.domains.keys().map(|s| s.as_str()).collect();
            eprintln!(
                "error: unknown domain '{}'. Available: {}",
                cli.domain,
                available.join(", ")
            );
            return ExitCode::FAILURE;
        }
    };

    if cli.verbose {
        eprintln!("[report] domain={}, period={since} to {until}", cli.domain);
        if let Some(ref user) = cli.user {
            eprintln!("[report] user={user}");
        }
    }

    // TODO: run reports based on domain config
    let _ = domain;
    eprintln!("sandogasa-report is not yet fully implemented");

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quarter_q1() {
        let (s, e) = parse_period("2026Q1").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
    }

    #[test]
    fn parse_quarter_q4() {
        let (s, e) = parse_period("2026Q4").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
    }

    #[test]
    fn parse_half_h1() {
        let (s, e) = parse_period("2026H1").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 6, 30).unwrap());
    }

    #[test]
    fn parse_half_h2() {
        let (s, e) = parse_period("2026H2").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
    }

    #[test]
    fn parse_period_case_insensitive() {
        let (s, _) = parse_period("2026q1").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
    }

    #[test]
    fn parse_period_invalid() {
        assert!(parse_period("2026X1").is_err());
        assert!(parse_period("abcd").is_err());
        assert!(parse_period("2026Q5").is_err());
    }
}
