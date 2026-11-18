// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use chrono::NaiveDate;
use clap::{Parser, Subcommand};

mod bodhi;
mod brace;
mod bugzilla;
mod by_repo;
mod config;
mod configure;
mod forge;
mod forgejo;
mod github;
mod gitlab;
mod koji;
mod notes;
mod report;
mod secrets;
mod sourcehut;

#[derive(Parser)]
#[command(
    about,
    long_about = None,
    max_term_width = 80,
    version = sandogasa_cli::version!(),
    before_help = sandogasa_cli::banner!()
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
    /// Record a note on why a piece of work was done.
    Note(NoteArgs),
    /// List recorded notes.
    Notes(NotesArgs),
    /// Generate an activity report across one or more domains.
    Report(ReportArgs),
}

#[derive(clap::Args)]
struct NoteArgs {
    /// The note; `-` reads it from standard input.
    #[arg(required = true, value_name = "TEXT")]
    text: Vec<String>,

    /// Who wrote the text, when not you (e.g. an assistant, as
    /// `Agent:model`).
    #[arg(long, value_name = "NAME")]
    by: Option<String>,

    /// The day the work was done (YYYY-MM-DD, default: today).
    #[arg(long)]
    date: Option<NaiveDate>,

    /// The web page of the item the note is about (a PR, bug or Bodhi
    /// update); leave it out for work no report lists.
    #[arg(long)]
    url: Option<String>,

    /// Write the note without showing it first.
    #[arg(short, long)]
    yes: bool,
}

#[derive(clap::Args)]
struct NotesArgs {
    /// Output as JSON.
    #[arg(long)]
    json: bool,

    /// Only notes from this day on (YYYY-MM-DD).
    #[arg(long)]
    since: Option<NaiveDate>,

    /// Only notes up to this day (YYYY-MM-DD).
    #[arg(long)]
    until: Option<NaiveDate>,

    /// Only notes about this URL.
    #[arg(long)]
    url: Option<String>,
}

/// An axis a report can be broken down along.
///
/// Only `repo` for now: a run is scoped to one person, so grouping
/// by user would yield a single group. Attributing a namespace's
/// activity per person is a different query shape and is recorded in
/// TODO.md rather than bolted on here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Axis {
    /// One row per repository, summed across domains and forges.
    Repo,
}

#[derive(clap::Args)]
struct ReportArgs {
    /// Domain(s) to report on (defined in config).
    #[arg(short, long, required = true, help_heading = "Selection")]
    domain: Vec<String>,

    /// Reporting period (e.g. 2026Q1, 2026H1).
    #[arg(long, group = "date_range", help_heading = "Selection")]
    period: Option<String>,

    /// Start date (inclusive, YYYY-MM-DD).
    #[arg(long, group = "date_range", help_heading = "Selection")]
    since: Option<NaiveDate>,

    /// End date (inclusive, YYYY-MM-DD, default: today).
    #[arg(long, requires = "since", help_heading = "Selection")]
    until: Option<NaiveDate>,

    /// FAS username to report on.
    #[arg(short, long, help_heading = "Selection")]
    user: Option<String>,

    /// Path to main config file (domains, groups).
    #[arg(short, long, value_name = "PATH", help_heading = "Sources")]
    config: Option<String>,

    /// Skip Bodhi queries.
    #[arg(long, help_heading = "Sources")]
    no_bodhi: bool,

    /// Skip Bugzilla queries.
    #[arg(long, help_heading = "Sources")]
    no_bugzilla: bool,

    /// Skip Forgejo queries.
    #[arg(long, help_heading = "Sources")]
    no_forgejo: bool,

    /// Skip GitHub queries.
    #[arg(long, help_heading = "Sources")]
    no_github: bool,

    /// Skip GitLab queries.
    #[arg(long, help_heading = "Sources")]
    no_gitlab: bool,

    /// Skip Koji queries.
    #[arg(long, help_heading = "Sources")]
    no_koji: bool,

    /// Skip Sourcehut queries.
    #[arg(long, help_heading = "Sources")]
    no_sourcehut: bool,

    /// Add a breakdown grouped this way (repeat or comma-separate).
    #[arg(
        long,
        value_name = "AXIS",
        value_delimiter = ',',
        help_heading = "Output"
    )]
    by: Vec<Axis>,

    /// Add detail. Once: counts per repo, project or component
    /// under each heading. Twice: the items themselves — every
    /// PR, bug, patch and ticket, and every build of a Bodhi
    /// update.
    #[arg(long, action = clap::ArgAction::Count, help_heading = "Output")]
    detailed: u8,

    /// Output as JSON instead of Markdown.
    #[arg(long, help_heading = "Output")]
    json: bool,

    /// Add the notes recorded with `note` in the period: each under
    /// the item it is about, the rest in a section of their own.
    #[arg(long, help_heading = "Output")]
    notes: bool,

    /// Write output to file instead of stdout.
    #[arg(short, long, value_name = "PATH", help_heading = "Output")]
    output: Option<String>,

    /// Make a report for your own use: with --notes, also the notes on
    /// items the report does not show, those without a URL, and
    /// embargoed ones. Without it, a note shows only under an item
    /// the report shows (with --json, any item it carries).
    #[arg(long, help_heading = "Output")]
    private: bool,

    /// Print progress to stderr.
    #[arg(short, long, help_heading = "Output")]
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

/// One Bugzilla instance's share of a run: the products and Fedora
/// versions of every domain that names it, and the CLI position of the
/// last one, where its section goes.
struct BugzillaAggregate {
    config: config::BugzillaConfig,
    fedora_versions: Vec<u32>,
    last_idx: usize,
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    match cli.command {
        Command::Config(args) => configure::run(&args),
        Command::Note(args) => report_error(run_note(&args)),
        Command::Notes(args) => report_error(run_notes(&args)),
        Command::Report(args) => run_report(&args),
    }
}

fn report_error(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The notes directory, from the user's config.
fn notes_dir() -> Result<std::path::PathBuf, String> {
    let cfg = config::load_config(None)?;
    notes::notes_dir(cfg.notes.dir.as_deref(), dirs::home_dir(), dirs::data_dir())
}

fn run_note(args: &NoteArgs) -> Result<(), String> {
    use std::io::IsTerminal;
    let dir = notes_dir()?;
    let host = notes::host()?;
    let text = notes::text_from(&args.text, &mut std::io::stdin())?;
    let now = chrono::Local::now().naive_local();
    let one_line = |name, v: &Option<String>| {
        v.as_deref()
            .map(|v| notes::header_value(name, v))
            .transpose()
    };
    let note = notes::Note {
        url: one_line("url", &args.url)?,
        date: args.date.unwrap_or(now.date()),
        by: one_line("by", &args.by)?,
        headers: Vec::new(),
        text,
    };
    // The text may have come from standard input; then the answer is
    // read from the terminal itself.
    let from_stdin = args.text.len() == 1 && args.text[0] == "-";
    let mut confirm = |preview: &str| -> Result<bool, String> {
        if args.yes {
            return Ok(true);
        }
        if !from_stdin && std::io::stdin().is_terminal() {
            eprint!("{preview}");
            return sandogasa_cli::confirm("Write this note?", true).map_err(|e| e.to_string());
        }
        let tty = std::io::stderr()
            .is_terminal()
            .then(|| std::fs::File::open("/dev/tty").ok())
            .flatten()
            .ok_or("a note is confirmed at a terminal; run it in one, or pass --yes")?;
        eprint!("{preview}Write this note? [Y/n]: ");
        let mut answer = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(tty), &mut answer)
            .map_err(|e| e.to_string())?;
        Ok(notes::answered_yes(&answer))
    };
    match notes::record(&dir, &note, now, &host, &mut confirm)? {
        Some(path) => println!("{}", path.display()),
        None => eprintln!("Nothing written."),
    }
    Ok(())
}

fn run_notes(args: &NotesArgs) -> Result<(), String> {
    let filter = notes::Filter {
        since: args.since,
        until: args.until,
        url: args.url.clone(),
    };
    notes::check_range(&filter)?;
    let (found, warnings) = notes::load_all(&notes_dir()?);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let selected = notes::select(found, &filter);
    if args.json {
        let json = serde_json::to_string_pretty(&selected).map_err(|e| e.to_string())?;
        println!("{json}");
    } else {
        print!("{}", notes::format_list(&selected));
    }
    Ok(())
}

/// The notes a report carries: all of them with --private, otherwise
/// only those under an item the report as read (`listed`) shows, with
/// a count of the rest on standard error.
fn shared_notes(
    cli: &ReportArgs,
    found: Vec<notes::Stored>,
    listed: impl FnOnce() -> String,
) -> Vec<notes::Stored> {
    if cli.private || found.is_empty() {
        return found;
    }
    let (kept, left_out) = notes::shareable(found, &listed());
    if left_out > 0 {
        eprintln!(
            "Left out {left_out} note(s) on items this report does not show, \
             without a URL, or embargoed; --private includes them."
        );
    }
    kept
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

    // The notes first: reading them is cheap, and a notes directory
    // that cannot be resolved should fail before the queries.
    let period_notes = if cli.notes {
        match notes::notes_dir(cfg.notes.dir.as_deref(), dirs::home_dir(), dirs::data_dir()) {
            Ok(dir) => {
                let (found, warnings) = notes::load_all(&dir);
                for w in &warnings {
                    eprintln!("warning: {w}");
                }
                notes::select(
                    found,
                    &notes::Filter {
                        since: Some(since),
                        until: Some(until),
                        url: None,
                    },
                )
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        Vec::new()
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
    // Bugzilla is aggregated per instance into a single query each;
    // we record the merged products and Fedora versions and the
    // position of the last domain that references the instance so
    // its section can be placed there. The header's "primary"
    // identity is the resolved FAS login — the profile key is a CLI
    // shorthand, not a username on any service, so rendering it
    // would be misleading.
    let mut domain_reports: Vec<report::DomainReport> = Vec::new();
    let mut block_cli_idx: Vec<usize> = Vec::new();
    let mut bugzillas: Vec<BugzillaAggregate> = Vec::new();

    for (cli_idx, (name, domain)) in domains.iter().enumerate() {
        // Bugzilla is aggregated; just record membership here.
        if let Some(bzc) = domain.bugzilla()
            && !cli.no_bugzilla
        {
            let agg = match bugzillas
                .iter()
                .position(|a| a.config.instance == bzc.instance)
            {
                Some(i) => &mut bugzillas[i],
                None => {
                    bugzillas.push(BugzillaAggregate {
                        config: config::BugzillaConfig {
                            products: Vec::new(),
                            ..bzc.clone()
                        },
                        fedora_versions: Vec::new(),
                        last_idx: 0,
                    });
                    bugzillas.last_mut().expect("just pushed")
                }
            };
            for p in &bzc.products {
                if !agg.config.products.contains(p) {
                    agg.config.products.push(p.clone());
                }
            }
            for &v in &domain.fedora_versions {
                if !agg.fedora_versions.contains(&v) {
                    agg.fedora_versions.push(v);
                }
            }
            agg.last_idx = cli_idx;
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

        // Forgejo (per-domain). With a token the search is the token
        // owner's and the username only labels the section; without
        // one the public search runs by this name. Resolution mirrors
        // the others (profile.forgejo[<host>] → profile.fas → raw
        // --user), and a domain with no resolvable name still reports
        // when there is a token.
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
    // One Bugzilla query per instance, across the domains that share
    // it. Red Hat's email comes from the profile or FASJSON; any other
    // instance's has to be on the profile, keyed by host.
    let mut bugzilla = Vec::new();
    if !bugzillas.is_empty() {
        if let Some(ref user) = fas_user {
            for agg in &mut bugzillas {
                agg.fedora_versions.sort();
                let host = agg.config.host();
                let email = if agg.config.is_redhat() {
                    bugzilla::resolve_email(user, bz_email_override, cli.verbose)
                } else {
                    profile
                        .and_then(|p| p.bugzilla_emails.get(&host).cloned())
                        .ok_or_else(|| {
                            format!(
                                "no Bugzilla email for {host}: set \
                                 `bugzilla_emails.\"{host}\"` on the user \
                                 profile in the config"
                            )
                        })
                };
                let email = match email {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                match rt.block_on(bugzilla::bugzilla_report(
                    &agg.config,
                    &email,
                    &agg.fedora_versions,
                    since,
                    until,
                    cli.verbose,
                )) {
                    Ok(mut bz_report) => {
                        // Place the section after the last domain block
                        // (in CLI order) that references this instance:
                        // count the rendered blocks falling at or before
                        // that domain's CLI position.
                        bz_report.after =
                            block_cli_idx.iter().filter(|&&i| i <= agg.last_idx).count();
                        bugzilla.push(bz_report);
                    }
                    Err(e) => {
                        eprintln!("error: bugzilla ({host}): {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        } else {
            eprintln!("warning: --user required for Bugzilla reporting, skipping");
        }
    }

    if domain_reports.is_empty() && bugzilla.is_empty() {
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
    };

    // Format output. A breakdown is appended rather than replacing
    // the sections, since it summarizes what they already say and a
    // reader usually wants both.
    let by_repo = cli.by.contains(&Axis::Repo);
    let output = if cli.json {
        let rows = by_repo.then(|| by_repo::gather(&unified));
        let mut json = serde_json::json!({
            "report": &unified,
            "by_repo": rows,
        });
        // Only when asked for, so the shape stays as it was otherwise.
        if cli.notes {
            // The JSON carries every item, as the report does at full
            // detail.
            let period_notes = shared_notes(cli, period_notes, || {
                report::format_markdown(&unified, 2, &cfg.groups)
            });
            let notes: Vec<notes::ReportNote> = period_notes
                .iter()
                .map(|stored| notes::ReportNote {
                    stored,
                    key: stored.note.url.as_deref().map(notes::url_key),
                })
                .collect();
            json["notes"] = serde_json::to_value(notes).expect("JSON serialization failed");
        }
        serde_json::to_string_pretty(&json).expect("JSON serialization failed")
    } else {
        let mut md = report::format_markdown(&unified, cli.detailed, &cfg.groups);
        if by_repo {
            md.push_str(&by_repo::format_markdown(&by_repo::gather(&unified)));
        }
        // Last, so each note sits under its item and the rest close
        // the report.
        if cli.notes {
            let period_notes = shared_notes(cli, period_notes, || md.clone());
            let (attached, rest) = notes::attach(&md, &period_notes);
            let rest: Vec<&notes::Stored> = rest.iter().map(|&i| &period_notes[i]).collect();
            md = attached;
            md.push_str(&notes::format_section(&rest));
        }
        md
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
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/sandogasa-report.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }

    use super::*;

    #[test]
    fn resolve_date_range_requires_since_or_period() {
        let cli = ReportArgs {
            by: Vec::new(),
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
            notes: false,
            output: None,
            private: false,
            verbose: false,
        };
        assert!(resolve_date_range(&cli).is_err());
    }

    #[test]
    fn resolve_date_range_accepts_period() {
        let cli = ReportArgs {
            by: Vec::new(),
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
            notes: false,
            output: None,
            private: false,
            verbose: false,
        };
        let (s, e) = resolve_date_range(&cli).unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
    }
}
