// SPDX-License-Identifier: MPL-2.0

use std::process::ExitCode;

use chrono::NaiveDate;
use clap::Parser;

mod bodhi;
mod brace;
mod bugzilla;
mod config;
mod koji;
mod report;

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
    /// Path to config file (domains, groups).
    #[arg(short, long, value_name = "PATH")]
    config: Option<String>,

    /// FAS username to report on.
    #[arg(short, long)]
    user: Option<String>,

    /// Domain(s) to report on (defined in config).
    #[arg(short, long, required = true)]
    domain: Vec<String>,

    /// Start date (inclusive, YYYY-MM-DD).
    #[arg(long, group = "date_range")]
    since: Option<NaiveDate>,

    /// End date (inclusive, YYYY-MM-DD, default: today).
    #[arg(long, requires = "since")]
    until: Option<NaiveDate>,

    /// Reporting period (e.g. 2026Q1, 2026H1).
    #[arg(long, group = "date_range")]
    period: Option<String>,

    /// Skip Bugzilla queries.
    #[arg(long)]
    no_bugzilla: bool,

    /// Skip Bodhi queries.
    #[arg(long)]
    no_bodhi: bool,

    /// Skip Koji queries.
    #[arg(long)]
    no_koji: bool,

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

/// Parse a period string like "2026", "2026Q1", or "2026H1" into a date range.
fn parse_period(period: &str) -> Result<(NaiveDate, NaiveDate), String> {
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

    let cfg = match config::load_config(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Resolve domains.
    let mut domains = Vec::new();
    for name in &cli.domain {
        match cfg.domains.get(name) {
            Some(d) => domains.push((name.as_str(), d)),
            None => {
                if cfg.domains.is_empty() {
                    eprintln!(
                        "error: no domains configured. \
                         Pass --config with a config file defining domains."
                    );
                } else {
                    let available: Vec<&str> = cfg.domains.keys().map(|s| s.as_str()).collect();
                    eprintln!(
                        "error: unknown domain '{name}'. Available: {}",
                        available.join(", ")
                    );
                }
                return ExitCode::FAILURE;
            }
        }
    }

    let domain_label = cli.domain.join(" + ");

    if cli.verbose {
        eprintln!("[report] domain={domain_label}, period={since} to {until}");
        if let Some(ref user) = cli.user {
            eprintln!("[report] user={user}");
        }
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create async runtime");

    // Build the unified report.
    let mut unified = report::Report {
        user: cli.user.clone(),
        domain: domain_label,
        since,
        until,
        bugzilla: None,
        bodhi: None,
        koji: None,
    };

    // Collect across all domains.
    let mut needs_bugzilla = false;
    let mut bodhi_domains: Vec<(&str, &config::DomainConfig)> = Vec::new();
    let mut all_koji_domains = Vec::new();
    let mut fedora_versions: Vec<u32> = Vec::new();

    for (name, domain) in &domains {
        if domain.bugzilla && !cli.no_bugzilla {
            needs_bugzilla = true;
            for &v in &domain.fedora_versions {
                if !fedora_versions.contains(&v) {
                    fedora_versions.push(v);
                }
            }
        }
        if domain.bodhi && !cli.no_bodhi {
            bodhi_domains.push((name, domain));
        }
        if !domain.koji_tags.is_empty() && !cli.no_koji {
            all_koji_domains.push(*domain);
        }
    }
    fedora_versions.sort();

    // Bugzilla reporting (only once, regardless of how many domains).
    if needs_bugzilla {
        if let Some(ref user) = cli.user {
            let email = match bugzilla::resolve_email(user, &cfg.users, cli.verbose) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            match rt.block_on(bugzilla::bugzilla_report(
                &email,
                &fedora_versions,
                since,
                until,
                cli.verbose,
            )) {
                Ok(bz_report) => {
                    unified.bugzilla = Some(bz_report);
                }
                Err(e) => {
                    eprintln!("error: bugzilla: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            eprintln!("warning: --user required for Bugzilla reporting, skipping");
        }
    }

    // Koji CBS reporting (merge across all domains).
    for domain in &all_koji_domains {
        match koji::koji_report(domain, cli.user.as_deref(), since, until, cli.verbose) {
            Ok(koji_report) => {
                if let Some(ref mut existing) = unified.koji {
                    // Merge packages from additional domains.
                    for (name, entry) in koji_report.packages {
                        existing
                            .packages
                            .entry(name)
                            .and_modify(|e| {
                                // Merge version maps.
                                for (distro, ver) in &entry.versions {
                                    e.versions
                                        .entry(distro.clone())
                                        .or_insert_with(|| ver.clone());
                                }
                            })
                            .or_insert(entry);
                    }
                } else {
                    unified.koji = Some(koji_report);
                }
            }
            Err(e) => {
                eprintln!("error: koji: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    // Bodhi reporting.
    if !bodhi_domains.is_empty() {
        if let Some(ref user) = cli.user {
            for (_, domain) in &bodhi_domains {
                match rt.block_on(bodhi::bodhi_report(user, domain, since, until, cli.verbose)) {
                    Ok(bodhi_report) => {
                        if let Some(ref mut existing) = unified.bodhi {
                            // Merge: add updates from additional domains.
                            existing.total_updates += bodhi_report.total_updates;
                            existing.total_builds += bodhi_report.total_builds;
                            for (release, updates) in bodhi_report.by_release {
                                existing
                                    .by_release
                                    .entry(release)
                                    .or_default()
                                    .extend(updates);
                            }
                        } else {
                            unified.bodhi = Some(bodhi_report);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: bodhi: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        } else {
            eprintln!("warning: --user required for Bodhi reporting, skipping");
        }
    }

    if unified.bugzilla.is_none() && unified.bodhi.is_none() && unified.koji.is_none() {
        eprintln!("No data sources configured for the selected domain(s).");
        return ExitCode::FAILURE;
    }

    // Format output.
    let output = if cli.json {
        serde_json::to_string_pretty(&unified).expect("JSON serialization failed")
    } else {
        report::format_markdown(&unified, cli.detailed, &cfg.groups)
    };

    // Write output.
    if let Some(ref path) = cli.output {
        if let Err(e) = std::fs::write(path, &output) {
            eprintln!("error: failed to write {path}: {e}");
            return ExitCode::FAILURE;
        }
        eprintln!("Wrote report to {path}");
    } else {
        print!("{output}");
    }

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
    fn parse_year() {
        let (s, e) = parse_period("2026").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
    }

    #[test]
    fn parse_period_invalid() {
        assert!(parse_period("2026X1").is_err());
        assert!(parse_period("abc").is_err());
        assert!(parse_period("2026Q5").is_err());
    }
}
