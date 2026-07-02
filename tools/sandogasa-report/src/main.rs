// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use chrono::NaiveDate;
use clap::{Parser, Subcommand};

mod bodhi;
mod brace;
mod bugzilla;
mod config;
mod configure;
mod forgejo;
mod github;
mod gitlab;
mod koji;
mod report;
mod sourcehut;

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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Interactively set per-user overrides in
    /// `~/.config/sandogasa-report/config.toml`.
    Config(configure::ConfigArgs),
    /// Generate an activity report across one or more domains.
    Report(ReportArgs),
}

#[derive(clap::Args)]
struct ReportArgs {
    /// Path to main config file (domains, groups).
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

    /// Skip GitLab queries.
    #[arg(long)]
    no_gitlab: bool,

    /// Skip GitHub queries.
    #[arg(long)]
    no_github: bool,

    /// Skip Forgejo queries.
    #[arg(long)]
    no_forgejo: bool,

    /// Skip Sourcehut queries.
    #[arg(long)]
    no_sourcehut: bool,

    /// Include per-item details. Repeat for deeper detail —
    /// level 1 (`--detailed`) lists each Bodhi update but
    /// summarizes multi-build ones as "N builds", level 2
    /// (`--detailed --detailed`) lists every build. Koji,
    /// GitLab, and Bugzilla ignore the difference between
    /// levels 1 and 2 (no deeper layer to expose).
    #[arg(long, action = clap::ArgAction::Count)]
    detailed: u8,

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

/// Resolve the date range from CLI args. Requires one of
/// `--since` or `--period`; unlike the shared
/// [`sandogasa_cli::date::resolve_date_range`], sandogasa-report
/// treats a fully-unbounded range as a user error.
fn resolve_date_range(cli: &ReportArgs) -> Result<(NaiveDate, NaiveDate), String> {
    if cli.since.is_none() && cli.period.is_none() {
        return Err("either --since or --period is required".to_string());
    }
    sandogasa_cli::date::resolve_date_range(cli.since, cli.until, cli.period.as_deref())
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Config(args) => configure::run(&args),
        Command::Report(args) => run_report(&args),
    }
}

fn run_report(cli: &ReportArgs) -> ExitCode {
    let (since, until) = match resolve_date_range(cli) {
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

    // Resolve the CLI --user into a profile (if one is defined in
    // config.users) plus a FAS login string for the FAS-based
    // services (Bugzilla/Bodhi/Koji). Unknown --user values fall
    // back to being treated as the FAS login directly.
    let profile_key = cli.user.as_deref();
    let profile = profile_key.and_then(|k| cfg.users.get(k));
    let fas_user: Option<String> = profile_key.map(|k| {
        profile
            .map(|p| p.fas_or(k))
            .unwrap_or_else(|| k.to_string())
    });
    let bz_email_override = profile.and_then(|p| p.bugzilla_email.as_deref());

    if cli.verbose {
        eprintln!("[report] domain={domain_label}, period={since} to {until}");
        if let Some(key) = profile_key {
            match (&fas_user, profile.is_some()) {
                (Some(fas), true) => eprintln!("[report] profile={key}, fas={fas}"),
                (Some(fas), false) => eprintln!("[report] user={fas} (no profile)"),
                _ => {}
            }
        }
    }

    let rt = tokio::runtime::Runtime::new().expect("failed to create async runtime");

    // Build one report block per domain, in CLI --domain order.
    // Bugzilla is aggregated across all domains into a single query;
    // we record the merged Fedora versions and the position of the
    // last domain that references it so the section can be placed
    // there. The header's "primary" identity is the resolved FAS
    // login — the profile key is a CLI shorthand, not a username on
    // any service, so rendering it would be misleading.
    let mut domain_reports: Vec<report::DomainReport> = Vec::new();
    let mut block_cli_idx: Vec<usize> = Vec::new();
    let mut fedora_versions: Vec<u32> = Vec::new();
    let mut last_bugzilla_idx: Option<usize> = None;

    for (cli_idx, (name, domain)) in domains.iter().enumerate() {
        // Bugzilla is aggregated; just record membership here.
        if domain.bugzilla && !cli.no_bugzilla {
            for &v in &domain.fedora_versions {
                if !fedora_versions.contains(&v) {
                    fedora_versions.push(v);
                }
            }
            last_bugzilla_idx = Some(cli_idx);
        }

        let mut dr = report::DomainReport {
            name: (*name).to_string(),
            bodhi: None,
            koji: None,
            gitlab: None,
            github: None,
            forgejo: None,
            sourcehut: None,
        };

        // Bodhi (per-domain).
        if domain.bodhi && !cli.no_bodhi {
            if let Some(ref user) = fas_user {
                match rt.block_on(bodhi::bodhi_report(user, domain, since, until, cli.verbose)) {
                    Ok(bodhi_report) => dr.bodhi = Some(bodhi_report),
                    Err(e) => {
                        eprintln!("error: bodhi: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                eprintln!("warning: --user required for Bodhi reporting, skipping");
            }
        }

        // Koji CBS (per-domain).
        if !domain.koji_tags.is_empty() && !cli.no_koji {
            match koji::koji_report(domain, fas_user.as_deref(), since, until, cli.verbose) {
                Ok(koji_report) => dr.koji = Some(koji_report),
                Err(e) => {
                    eprintln!("error: koji: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }

        // GitLab (per-domain). Username resolution:
        // profile.gitlab[<host>] → profile.fas → raw --user.
        // Domains with no resolvable username are skipped.
        if let Some(gl) = domain.gitlab.as_ref()
            && !cli.no_gitlab
        {
            let host = gitlab::instance_host(&gl.instance);
            let resolved = profile
                .and_then(|p| p.gitlab_username(&host))
                .map(String::from)
                .or_else(|| fas_user.clone());
            match resolved {
                Some(user) => {
                    match gitlab::gitlab_report(
                        gl,
                        &user,
                        since,
                        until,
                        &cfg.gitlab_tokens,
                        cli.verbose,
                    ) {
                        Ok(gl_report) => dr.gitlab = Some(gl_report),
                        Err(e) => {
                            eprintln!("error: gitlab ({name}): {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                None => eprintln!(
                    "warning: GitLab domain '{name}' has no user — \
                     set --user, or add [users.<name>.gitlab.\"{host}\"] \
                     to the config, skipping"
                ),
            }
        }

        // GitHub (per-domain). Same resolution as GitLab.
        if let Some(gh) = domain.github.as_ref()
            && !cli.no_github
        {
            let host = github::instance_host(&gh.instance);
            let resolved = profile
                .and_then(|p| p.github_username(&host))
                .map(String::from)
                .or_else(|| fas_user.clone());
            match resolved {
                Some(user) => {
                    match github::github_report(
                        gh,
                        &user,
                        since,
                        until,
                        &cfg.github_tokens,
                        cli.verbose,
                    ) {
                        Ok(gh_report) => dr.github = Some(gh_report),
                        Err(e) => {
                            eprintln!("error: github ({name}): {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                None => eprintln!(
                    "warning: GitHub domain '{name}' has no user — \
                     set --user, or add [users.<name>.github.\"{host}\"] \
                     to the config, skipping"
                ),
            }
        }

        // Forgejo (per-domain). The search is token-scoped, so the
        // username is only for display; resolution mirrors the others
        // (profile.forgejo[<host>] → profile.fas → raw --user) and a
        // domain with no resolvable name still reports (it labels the
        // section with whatever name we have, or the token owner).
        if let Some(fj) = domain.forgejo.as_ref()
            && !cli.no_forgejo
        {
            let host = forgejo::instance_host(&fj.instance);
            let user = profile
                .and_then(|p| p.forgejo_username(&host))
                .map(String::from)
                .or_else(|| fas_user.clone())
                .unwrap_or_default();
            match forgejo::forgejo_report(fj, &user, since, until, &cfg.forgejo_tokens, cli.verbose)
            {
                Ok(fj_report) => dr.forgejo = Some(fj_report),
                Err(e) => {
                    eprintln!("error: forgejo ({name}): {e}");
                    return ExitCode::FAILURE;
                }
            }
        }

        if let Some(sh) = domain.sourcehut.as_ref()
            && !cli.no_sourcehut
        {
            let host = sourcehut::instance_host(&sh.instance);
            let user = profile
                .and_then(|p| p.sourcehut_username(&host))
                .map(String::from)
                .or_else(|| fas_user.clone())
                .unwrap_or_default();
            let owner_emails = profile.map(|p| p.git_emails.clone()).unwrap_or_default();
            match sourcehut::sourcehut_report(
                sh,
                &user,
                since,
                until,
                &cfg.sourcehut_tokens,
                &owner_emails,
                cli.verbose,
            ) {
                Ok(sh_report) => dr.sourcehut = Some(sh_report),
                Err(e) => {
                    eprintln!("error: sourcehut ({name}): {e}");
                    return ExitCode::FAILURE;
                }
            }
        }

        if dr.has_content() {
            domain_reports.push(dr);
            block_cli_idx.push(cli_idx);
        }
    }
    fedora_versions.sort();

    // Aggregated Bugzilla query (one per run, across all domains
    // that enable it).
    let mut bugzilla = None;
    if last_bugzilla_idx.is_some() {
        if let Some(ref user) = fas_user {
            let email = match bugzilla::resolve_email(user, bz_email_override, cli.verbose) {
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
                Ok(bz_report) => bugzilla = Some(bz_report),
                Err(e) => {
                    eprintln!("error: bugzilla: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            eprintln!("warning: --user required for Bugzilla reporting, skipping");
        }
    }

    // Place the aggregated Bugzilla section after the last domain
    // block (in CLI order) that references it: count the rendered
    // blocks falling at or before that domain's CLI position.
    let bugzilla_after = match last_bugzilla_idx {
        Some(idx) if bugzilla.is_some() => block_cli_idx.iter().filter(|&&i| i <= idx).count(),
        _ => 0,
    };

    if domain_reports.is_empty() && bugzilla.is_none() {
        eprintln!("No data sources configured for the selected domain(s).");
        return ExitCode::FAILURE;
    }

    let unified = report::Report {
        user: fas_user.clone(),
        domain: domain_label,
        since,
        until,
        domains: domain_reports,
        bugzilla,
        bugzilla_after,
    };

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
    fn resolve_date_range_requires_since_or_period() {
        let cli = ReportArgs {
            config: None,
            user: None,
            domain: vec![],
            since: None,
            until: None,
            period: None,
            no_bugzilla: false,
            no_bodhi: false,
            no_koji: false,
            no_gitlab: false,
            no_github: false,
            no_forgejo: false,
            no_sourcehut: false,
            detailed: 0,
            json: false,
            output: None,
            verbose: false,
        };
        assert!(resolve_date_range(&cli).is_err());
    }

    #[test]
    fn resolve_date_range_accepts_period() {
        let cli = ReportArgs {
            config: None,
            user: None,
            domain: vec![],
            since: None,
            until: None,
            period: Some("2026Q1".into()),
            no_bugzilla: false,
            no_bodhi: false,
            no_koji: false,
            no_gitlab: false,
            no_github: false,
            no_forgejo: false,
            no_sourcehut: false,
            detailed: 0,
            json: false,
            output: None,
            verbose: false,
        };
        let (s, e) = resolve_date_range(&cli).unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
    }
}
