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
mod debbugs;
mod forge;
mod forgejo;
mod github;
mod gitlab;
mod koji;
mod launchpad;
mod notes;
mod pagure;
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

    /// The notes directory to write to, one of those configured
    /// (default: the first public one, or for an embargoed note the
    /// first private one).
    #[arg(long, value_name = "PATH")]
    dir: Option<String>,

    /// Mark the note embargoed until a day (YYYY-MM-DD) or `disclosed`;
    /// it goes only in a private notes directory.
    #[arg(long, value_name = "UNTIL")]
    embargo: Option<String>,

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
    /// Only notes from this day on (YYYY-MM-DD).
    #[arg(long, help_heading = "Selection")]
    since: Option<NaiveDate>,

    /// Only notes up to this day (YYYY-MM-DD).
    #[arg(long, help_heading = "Selection")]
    until: Option<NaiveDate>,

    /// Only notes about this URL.
    #[arg(long, help_heading = "Selection")]
    url: Option<String>,

    /// Move the notes in private directories whose embargo has lifted
    /// to the first public one, after asking.
    #[arg(long, conflicts_with = "rewrite_moved", help_heading = "Writing")]
    release: bool,

    /// Point the url: of notes on moved repositories at where they are
    /// now, from [notes.moved] and GitHub, after asking.
    #[arg(long, help_heading = "Writing")]
    rewrite_moved: bool,

    /// With --release or --rewrite-moved, write without asking.
    #[arg(short, long, help_heading = "Writing")]
    yes: bool,

    /// Output as JSON.
    #[arg(long, conflicts_with_all = ["release", "rewrite_moved"], help_heading = "Output")]
    json: bool,

    /// Print the link and text of embargoed notes, which are otherwise
    /// hidden so a screen or a pasted log does not give the issue away.
    #[arg(long, help_heading = "Output")]
    show_embargoed: bool,
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

    /// Skip Debian bug tracker queries.
    #[arg(long, help_heading = "Sources")]
    no_bts: bool,

    /// Skip Bugzilla queries.
    #[arg(long, help_heading = "Sources")]
    no_bugzilla: bool,

    /// Skip Forgejo/Gitea queries.
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

    /// Skip Launchpad queries.
    #[arg(long, help_heading = "Sources")]
    no_launchpad: bool,

    /// Skip Pagure queries.
    #[arg(long, help_heading = "Sources")]
    no_pagure: bool,

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

    /// Make a report for your own use: include GitHub work in private
    /// repositories and, with --notes, the notes on items the report
    /// does not show, those without a URL, and embargoed ones. Without
    /// it, the GitHub section keeps to public repositories and a note
    /// shows only under an item the report shows (with --json, any
    /// item it carries). The other forges' sections include private
    /// projects your tokens can see either way.
    #[arg(long, help_heading = "Output")]
    private: bool,

    /// With --notes and --private, print the link and text of embargoed
    /// notes, which are otherwise hidden so a screen or a pasted log
    /// does not give the issue away.
    #[arg(long, requires_all = ["notes", "private"], help_heading = "Output")]
    show_embargoed: bool,

    /// With --notes and --private, what the Markdown report does with
    /// the notes no item took (default: [notes] unattached in the
    /// config, or include). --json keeps them, marked unattached.
    #[arg(long, value_name = "WHAT", requires = "notes", help_heading = "Output")]
    unattached_notes: Option<notes::Unattached>,

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

/// The directories notes are read from, from `cfg`.
fn note_dirs(cfg: &config::NotesConfig) -> Result<Vec<notes::NoteDir>, String> {
    let more: Vec<&str> = cfg.dirs.iter().map(String::as_str).collect();
    let private: Vec<&str> = cfg.private.iter().map(String::as_str).collect();
    notes::note_dirs(
        cfg.dir.as_deref(),
        &more,
        &private,
        dirs::home_dir(),
        dirs::data_dir(),
    )
}

fn run_note(args: &NoteArgs) -> Result<(), String> {
    use std::io::IsTerminal;
    let cfg = config::load_config(None)?;
    let dirs = note_dirs(&cfg.notes)?;
    let home = dirs::home_dir();
    let dir = &notes::write_dir(
        &dirs,
        args.dir.as_deref(),
        args.embargo.is_some(),
        home.as_deref(),
    )?
    .path;
    let embargo = args
        .embargo
        .as_deref()
        .map(notes::embargo_value)
        .transpose()?;
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
        headers: embargo.map(|v| ("embargo".into(), v)).into_iter().collect(),
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
    match notes::record(dir, &note, now, &host, &mut confirm)? {
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
    if args.yes && !(args.release || args.rewrite_moved) {
        return Err("--yes goes with --release or --rewrite-moved".into());
    }
    let cfg = config::load_config(None)?;
    let dirs = note_dirs(&cfg.notes)?;
    let (found, warnings) = notes::load_dirs(&dirs);
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let selected = notes::select(found, &filter);
    if args.release {
        return release_notes(&dirs, selected, args.yes, args.show_embargoed);
    }
    if args.rewrite_moved {
        return rewrite_notes(&cfg, selected, args.yes, args.show_embargoed);
    }
    let selected: Vec<notes::Stored> = match args.show_embargoed {
        true => selected,
        false => notes::hide_embargoed(selected, chrono::Local::now().date_naive())
            .iter()
            .map(notes::shown)
            .collect(),
    };
    if args.json {
        let json = serde_json::to_string_pretty(&selected).map_err(|e| e.to_string())?;
        println!("{json}");
    } else {
        print!("{}", notes::format_list(&selected));
    }
    Ok(())
}

/// The notes `notes --release` asks about: in full with `show`,
/// otherwise each by its date and file, since they were secret until
/// now and the file's name gives nothing away.
fn release_list(lifted: &[notes::Stored], show: bool) -> String {
    match show {
        true => notes::format_list(lifted),
        false => lifted
            .iter()
            .map(|s| format!("{} {}\n", s.note.date, s.path.display()))
            .collect(),
    }
}

/// `notes --rewrite-moved`: point the `url:` of each note among
/// `selected` on a moved repository at where it is now, from
/// `[notes.moved]` and one GitHub lookup for the github.com
/// repositories the notes name after the table's moves; after asking unless `yes`. Embargoed notes are
/// counted rather than listed unless `show`, and are never part of the
/// lookup. The moves GitHub found are offered for the config first, for
/// notes written later from old links, whether or not the files are
/// then rewritten.
fn rewrite_notes(
    cfg: &config::ReportConfig,
    selected: Vec<notes::Stored>,
    yes: bool,
    show: bool,
) -> Result<(), String> {
    use std::io::IsTerminal;
    let today = chrono::Local::now().date_naive();
    let mut moved = cfg.notes.moved.clone();
    // GitHub is asked where the notes' repositories are after the
    // table's moves, so one that moved again since is followed too.
    let mut resolved = selected.clone();
    notes::apply_moves(&mut resolved, &moved);
    let all: Vec<usize> = (0..resolved.len()).collect();
    const GITHUB: &str = "https://api.github.com";
    let lookup = |repos: &[String]| {
        let token = github::find_token(GITHUB, &cfg.github_tokens)?;
        eprintln!(
            "Checking with GitHub whether {} repo(s) of notes moved...",
            repos.len()
        );
        sandogasa_github::Client::new(GITHUB, &token)
            .and_then(|c| c.current_repo_names(repos))
            .map_err(|e| e.to_string())
    };
    let found = notes::github_moves(&resolved, &all, today, lookup).unwrap_or_else(|e| {
        eprintln!("warning: GitHub not asked whether repositories moved: {e}");
        Default::default()
    });
    moved.extend(found.clone());
    let at_terminal = std::io::stdin().is_terminal();
    // What GitHub found is worth keeping whatever happens to the files,
    // so it is offered first.
    if !found.is_empty() {
        let found: Vec<(String, String)> = found.into_iter().collect();
        record_moves(&found, at_terminal && !yes);
    }
    let plan = notes::planned_rewrites(&selected, &moved);
    if plan.is_empty() {
        eprintln!("No note names a moved repository.");
        return Ok(());
    }
    print!("{}", notes::format_rewrites(&selected, &plan, today, show));
    let question = format!("Rewrite the url: of these {} note(s)?", plan.len());
    if !yes {
        if !at_terminal {
            return Err(format!(
                "{question} Rewriting is confirmed at a terminal; run it in one, or pass --yes"
            ));
        }
        if !sandogasa_cli::confirm(&question, true).map_err(|e| e.to_string())? {
            eprintln!("Nothing rewritten.");
            return Ok(());
        }
    }
    // Each note on its own: one that cannot be rewritten stays, and the
    // rest are still done.
    let mut failed = 0;
    for (i, new) in &plan {
        let stored = &selected[*i];
        match notes::rewrite_url(&stored.path, new) {
            Ok(()) => println!("{}", stored.path.display()),
            Err(e) => {
                eprintln!("error: {e}");
                failed += 1;
            }
        }
    }
    match failed {
        0 => Ok(()),
        n => Err(format!("{n} note(s) could not be rewritten")),
    }
}

/// `notes --release`: move the notes among `selected` that are in a
/// private directory and no longer embargoed to the first public
/// directory, after asking unless `yes`. The list asked about hides
/// their links and text unless `show`: they were secret until now.
fn release_notes(
    dirs: &[notes::NoteDir],
    selected: Vec<notes::Stored>,
    yes: bool,
    show: bool,
) -> Result<(), String> {
    use std::io::IsTerminal;
    let today = chrono::Local::now().date_naive();
    let lifted: Vec<notes::Stored> = selected
        .into_iter()
        .filter(|s| s.private && notes::lifted(&s.note, today))
        .collect();
    if lifted.is_empty() {
        eprintln!("No note in a private directory has had its embargo lifted.");
        return Ok(());
    }
    let to = &dirs
        .iter()
        .find(|d| !d.private)
        .ok_or("[notes] lists no public directory to move them to")?
        .path;
    print!("{}", release_list(&lifted, show));
    let question = format!("Move these {} note(s) to {}?", lifted.len(), to.display());
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(format!(
                "{question} Moving is confirmed at a terminal; run it in one, or pass --yes"
            ));
        }
        if !sandogasa_cli::confirm(&question, true).map_err(|e| e.to_string())? {
            eprintln!("Nothing moved.");
            return Ok(());
        }
    }
    // Each note on its own: one that cannot be moved stays, and the
    // rest still go.
    let mut failed = 0;
    for stored in &lifted {
        match notes::release(stored, to, today) {
            Ok(dest) => println!("{}", dest.display()),
            Err(e) => {
                eprintln!("error: {e}");
                failed += 1;
            }
        }
    }
    match failed {
        0 => Ok(()),
        n => Err(format!(
            "{n} of {} note(s) could not be moved, and stay where they were",
            lifted.len()
        )),
    }
}

/// Whether to close the report with the `count` notes no item took:
/// as `--unattached-notes` says, else the config's `[notes]
/// unattached`, else yes; `ask` asks with `confirm` at a terminal and
/// says yes without one.
fn include_unattached(
    cli: &ReportArgs,
    configured: Option<notes::Unattached>,
    count: usize,
    at_terminal: bool,
    confirm: impl FnOnce(&str) -> bool,
) -> bool {
    match cli.unattached_notes.or(configured).unwrap_or_default() {
        notes::Unattached::Include => true,
        notes::Unattached::Exclude => {
            eprintln!("Left out {count} note(s) on items not in this report.");
            false
        }
        notes::Unattached::Ask if at_terminal => confirm(&format!(
            "Add the {count} note(s) on items not in this report?"
        )),
        notes::Unattached::Ask => true,
    }
}

/// The notes a report carries: all of them with --private, otherwise
/// only those under an item the report as read (`listed`) shows and
/// not in a private directory, with a count of the rest on standard
/// error.
fn shared_notes(
    cli: &ReportArgs,
    found: Vec<notes::Stored>,
    listed: impl FnOnce() -> String,
) -> Vec<notes::Stored> {
    if cli.private || found.is_empty() {
        return found;
    }
    let (public, private): (Vec<_>, Vec<_>) = found.into_iter().partition(|s| !s.private);
    let (kept, left_out) = match public.is_empty() {
        true => (public, 0),
        false => notes::shareable(public, &listed(), chrono::Local::now().date_naive()),
    };
    let left_out = left_out + private.len();
    if left_out > 0 {
        eprintln!(
            "Left out {left_out} note(s) on items this report does not show, \
             without a URL, embargoed, or from a private directory; --private \
             includes them."
        );
    }
    kept
}

/// The notes a report carries ([`shared_notes`]), with the embargoed
/// ones among them hidden unless --show-embargoed.
fn report_notes(
    cli: &ReportArgs,
    found: Vec<notes::Stored>,
    listed: impl FnOnce() -> String,
) -> Vec<notes::Stored> {
    let carried = shared_notes(cli, found, listed);
    match cli.show_embargoed {
        true => carried,
        false => notes::hide_embargoed(carried, chrono::Local::now().date_naive()),
    }
}

/// `notes` pointed at where their items are now: by the config's
/// `[notes.moved]`, then, when the report read github.com, by asking
/// GitHub in one request where the repositories of the notes no item in
/// `listed` takes have gone.
fn follow_moves(
    mut found: Vec<notes::Stored>,
    listed: &str,
    cfg: &config::ReportConfig,
    reads_github: bool,
    verbose: bool,
    interactive: bool,
) -> Vec<notes::Stored> {
    notes::apply_moves(&mut found, &cfg.notes.moved);
    if !reads_github {
        return found;
    }
    const GITHUB: &str = "https://api.github.com";
    let lookup = |repos: &[String]| {
        let token = github::find_token(GITHUB, &cfg.github_tokens)?;
        eprintln!(
            "Checking with GitHub whether {} repo(s) of unattached notes moved...",
            repos.len()
        );
        if verbose {
            eprintln!("[notes] GitHub GraphQL: {}", repos.join(", "));
        }
        sandogasa_github::Client::new(GITHUB, &token)
            .and_then(|c| c.current_repo_names(repos))
            .map_err(|e| e.to_string())
    };
    let today = chrono::Local::now().date_naive();
    match notes::follow_github_moves(&mut found, listed, today, lookup) {
        Ok(moves) if moves.is_empty() => {}
        Ok(moves) => record_moves(&moves, interactive),
        Err(e) => eprintln!("warning: cannot check whether notes' repositories moved: {e}"),
    }
    found
}

/// Say which repositories GitHub found moved and, at a terminal, offer
/// to add them to `[notes.moved]` in the user config, which keeps their
/// notes matched even once GitHub no longer redirects the old name;
/// otherwise print the lines to add.
fn record_moves(moves: &[(String, String)], interactive: bool) {
    for (old, new) in moves {
        eprintln!("{old} has moved to {new}");
    }
    let cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
    let question = format!("Add to [notes.moved] in {}?", cf.path().display());
    if interactive && sandogasa_cli::confirm(&question, true).unwrap_or(false) {
        match notes::save_moves(&cf, moves) {
            Ok(()) => eprintln!("Saved {} move(s) to {}.", moves.len(), cf.path().display()),
            Err(e) => eprintln!("warning: {e}"),
        }
        return;
    }
    eprintln!(
        "To match notes that name the old repositories without asking GitHub, add to the \
         config:\n\n[notes.moved]"
    );
    for (old, new) in moves {
        eprintln!("\"{old}\" = \"{new}\"");
    }
    eprintln!();
}

/// Whether a source's error says its token was refused or lacks a
/// scope: an HTTP 401 or 403, as the clients print a status with its
/// reason, or Forgejo's "required scope(s)". Not the bare digits, which
/// an issue number in the error could hold.
fn token_refused(error: &str) -> bool {
    ["401 Unauthorized", "403 Forbidden", "required scope"]
        .iter()
        .any(|sign| error.contains(sign))
}

/// Record that the source `what` failed with `error` and is left out of
/// the report, saying so; for a source read with a token on
/// `token_host` that was refused, how to replace it.
fn leave_out(left_out: &mut Vec<String>, what: String, error: &str, token_host: Option<&str>) {
    eprintln!("warning: {what}: {error} — left out of the report");
    if let Some(host) = token_host
        && token_refused(error)
    {
        eprintln!(
            "  `sandogasa-report config --token {host}` replaces the token, and says \
             what it needs"
        );
    }
    left_out.push(format!("{what}: {error}"));
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
        match note_dirs(&cfg.notes) {
            Ok(dirs) => {
                let (found, warnings) = notes::load_dirs(&dirs);
                for w in &warnings {
                    eprintln!("warning: {w}");
                }
                let today = chrono::Local::now().date_naive();
                let waiting = found
                    .iter()
                    .filter(|s| s.private && notes::lifted(&s.note, today))
                    .count();
                if waiting > 0 {
                    eprintln!(
                        "{waiting} note(s) in a private directory are no longer \
                         embargoed; `sandogasa-report notes --release` moves them."
                    );
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
    // Sources that failed: the rest is still reported, saying which.
    let mut left_out: Vec<String> = Vec::new();
    // A user's GitLab activity, read once per instance for the domains
    // on it.
    let mut gitlab_activity = gitlab::Activity::default();
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
            pagure: None,
            bts: None,
            launchpad: None,
        };

        // Bodhi (per-domain).
        if domain.bodhi && !cli.no_bodhi {
            if let Some(ref user) = fas_user {
                eprintln!("Reading the Bodhi updates of {user}...");
                match rt.block_on(bodhi::bodhi_report(user, domain, since, until, cli.verbose)) {
                    Ok(bodhi_report) => dr.bodhi = Some(bodhi_report),
                    Err(e) => leave_out(&mut left_out, format!("bodhi ({name})"), &e, None),
                }
            } else {
                eprintln!("warning: --user required for Bodhi reporting, skipping");
            }
        }

        // Koji CBS (per-domain).
        if !domain.koji_tags.is_empty() && !cli.no_koji {
            eprintln!("Reading the builds in {name}'s Koji tags...");
            match koji::koji_report(domain, fas_user.as_deref(), since, until, cli.verbose) {
                Ok(koji_report) => dr.koji = Some(koji_report),
                Err(e) => leave_out(&mut left_out, format!("koji ({name})"), &e, None),
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
                    eprintln!("Reading the GitLab activity of {user} on {host}...");
                    match gitlab::gitlab_report(
                        gl,
                        &user,
                        since,
                        until,
                        &cfg.gitlab_tokens,
                        &mut gitlab_activity,
                        cli.verbose,
                    ) {
                        Ok(gl_report) => dr.gitlab = Some(gl_report),
                        Err(e) => {
                            leave_out(&mut left_out, format!("gitlab ({name})"), &e, Some(&host))
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
                    eprintln!("Reading the GitHub activity of {user} on {host}...");
                    match github::github_report(
                        gh,
                        &user,
                        since,
                        until,
                        &cfg.github_tokens,
                        cli.private,
                        cli.verbose,
                    ) {
                        Ok(gh_report) => dr.github = Some(gh_report),
                        Err(e) => {
                            leave_out(&mut left_out, format!("github ({name})"), &e, Some(&host))
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
            if user.is_empty() && forgejo::find_token(&fj.instance, &cfg.forgejo_tokens).is_err() {
                eprintln!(
                    "warning: Forgejo/Gitea domain '{name}' has no user and no token for \
                     {host} — set --user, or store a token with `sandogasa-report config`, \
                     skipping"
                );
            } else {
                match forgejo::forgejo_report(
                    fj,
                    &user,
                    since,
                    until,
                    &cfg.forgejo_tokens,
                    cli.verbose,
                ) {
                    Ok(fj_report) => dr.forgejo = Some(fj_report),
                    Err(e) => {
                        leave_out(&mut left_out, format!("forgejo ({name})"), &e, Some(&host))
                    }
                }
            }
        }

        // Pagure (per-domain): answered anonymously, by the profile's
        // login for the instance, else the FAS name (the login on
        // Fedora's dist-git).
        if let Some(pg) = domain.pagure.as_ref()
            && !cli.no_pagure
        {
            let host = pagure::instance_host(&pg.instance);
            let login = profile
                .and_then(|p| p.pagure_username(&host))
                .map(String::from)
                .or_else(|| fas_user.clone());
            match login.as_deref() {
                Some(user) => match pagure::pagure_report(pg, user, since, until, &rt, cli.verbose)
                {
                    Ok(pg_report) => dr.pagure = Some(pg_report),
                    Err(e) => leave_out(&mut left_out, format!("pagure ({name})"), &e, None),
                },
                None => {
                    eprintln!("warning: Pagure domain '{name}' has no user — set --user, skipping")
                }
            }
        }

        // The Debian bug tracker (per-domain), by the profile's Debian
        // addresses.
        if domain.bts && !cli.no_bts {
            let emails = profile.map(|p| p.debian_emails.clone()).unwrap_or_default();
            if emails.is_empty() {
                eprintln!(
                    "warning: domain '{name}' reads the Debian bug tracker, but the profile \
                     names no debian_emails — `sandogasa-report config --missing`, skipping"
                );
            } else {
                eprintln!("Reading the Debian bug tracker for your Debian addresses...");
                match debbugs::bts_report(
                    debbugs::DEFAULT_INSTANCE,
                    &emails,
                    since,
                    until,
                    cli.verbose,
                ) {
                    Ok(bts_report) => dr.bts = Some(bts_report),
                    Err(e) => leave_out(
                        &mut left_out,
                        format!("Debian bug tracker ({name})"),
                        &e,
                        None,
                    ),
                }
            }
        }

        // Launchpad (per-domain): the PPA uploads of the profile's
        // Launchpad login, read anonymously.
        if domain.reads_launchpad() && !cli.no_launchpad {
            match profile.and_then(|p| p.launchpad.clone()) {
                None if profile.is_none() => eprintln!(
                    "warning: domain '{name}' reads Launchpad, which needs a profile \
                     (--user) with a launchpad login, skipping"
                ),
                None => eprintln!(
                    "warning: domain '{name}' reads Launchpad, but the profile names no \
                     launchpad login — `sandogasa-report config --missing`, skipping"
                ),
                Some(login) => match launchpad::launchpad_report(
                    launchpad::DEFAULT_INSTANCE,
                    &login,
                    &domain.launchpad_ppas,
                    since,
                    until,
                    cli.verbose,
                ) {
                    Ok(lp_report) => dr.launchpad = Some(lp_report),
                    Err(e) => leave_out(&mut left_out, format!("launchpad ({name})"), &e, None),
                },
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
            eprintln!("Reading the Sourcehut activity on {host}...");
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
                Err(e) => leave_out(
                    &mut left_out,
                    format!("sourcehut ({name})"),
                    &e,
                    Some(&host),
                ),
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
                        leave_out(&mut left_out, format!("bugzilla ({host})"), &e, None);
                        continue;
                    }
                };
                eprintln!("Searching {host} for the bugs of {email}...");
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
                    Err(e) => leave_out(&mut left_out, format!("bugzilla ({host})"), &e, None),
                }
            }
        } else {
            eprintln!("warning: --user required for Bugzilla reporting, skipping");
        }
    }

    if domain_reports.is_empty() && bugzilla.is_empty() {
        match left_out.is_empty() {
            true => eprintln!("No data sources configured for the selected domain(s)."),
            false => eprintln!("Every source failed: nothing to report."),
        }
        return ExitCode::FAILURE;
    }

    let unified = report::Report {
        user: fas_user.clone(),
        domain: domain_label,
        since,
        until,
        domains: domain_reports,
        bugzilla,
        left_out: left_out.clone(),
    };

    // Format output. A breakdown is appended rather than replacing
    // the sections, since it summarizes what they already say and a
    // reader usually wants both.
    let by_repo = cli.by.contains(&Axis::Repo);
    let reads_github = !cli.no_github
        && domains.iter().any(|(_, d)| {
            d.github
                .as_ref()
                .is_some_and(|g| github::instance_host(&g.instance) == "api.github.com")
        });
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
            // As the Markdown report would list them at full detail.
            let mut listed = report::format_markdown(&unified, 2, &cfg.groups);
            if by_repo {
                listed.push_str(&by_repo::format_markdown(&by_repo::gather(&unified)));
            }
            let period_notes = follow_moves(
                period_notes,
                &listed,
                &cfg,
                reads_github,
                cli.verbose,
                false,
            );
            let period_notes = report_notes(cli, period_notes, || listed.clone());
            let (_, rest) = notes::attach(&listed, &period_notes);
            let printed: Vec<notes::Stored> = period_notes.iter().map(notes::shown).collect();
            let notes: Vec<notes::ReportNote> = printed
                .iter()
                .enumerate()
                .map(|(i, stored)| notes::ReportNote {
                    stored,
                    key: stored.note.url.as_deref().map(notes::url_key),
                    attached: !rest.contains(&i),
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
            let period_notes = follow_moves(
                period_notes,
                &md,
                &cfg,
                reads_github,
                cli.verbose,
                std::io::IsTerminal::is_terminal(&std::io::stdin()),
            );
            let period_notes = report_notes(cli, period_notes, || md.clone());
            let (attached, rest) = notes::attach(&md, &period_notes);
            let rest: Vec<notes::Stored> = rest
                .iter()
                .map(|&i| notes::shown(&period_notes[i]))
                .collect();
            let rest: Vec<&notes::Stored> = rest.iter().collect();
            md = attached;
            let at_terminal = {
                use std::io::IsTerminal;
                std::io::stdin().is_terminal()
            };
            let confirm = |q: &str| sandogasa_cli::confirm(q, true).unwrap_or(true);
            if !rest.is_empty()
                && include_unattached(cli, cfg.notes.unattached, rest.len(), at_terminal, confirm)
            {
                md.push_str(&notes::format_section(&rest, cli.detailed));
            }
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

    // Written, but not whole: say so, and in the exit status.
    if !left_out.is_empty() {
        eprintln!(
            "warning: {} source(s) left out after errors; the report lists them",
            left_out.len()
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    /// A failed source is recorded with its error, and a refused token
    /// is told apart from other failures.
    #[test]
    fn a_failed_source_is_left_out_and_recorded() {
        assert!(token_refused("HTTP 401 Unauthorized"));
        assert!(token_refused(
            "HTTP 403 Forbidden: token does not have at least one of required scope(s): [read:issue]"
        ));
        assert!(!token_refused("HTTP 502 Bad Gateway"));
        assert!(!token_refused("issue #403 has no title"));
        let mut left = Vec::new();
        leave_out(
            &mut left,
            "forgejo (fedora)".into(),
            "HTTP 403",
            Some("forge.example"),
        );
        leave_out(&mut left, "bodhi (fedora)".into(), "timed out", None);
        assert_eq!(
            left,
            ["forgejo (fedora): HTTP 403", "bodhi (fedora): timed out"]
        );
    }

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

    fn report_args(extra: &[&str]) -> ReportArgs {
        use clap::Parser;
        let mut argv = vec![
            "sandogasa-report",
            "report",
            "-d",
            "x",
            "--period",
            "2026H2",
        ];
        argv.extend_from_slice(extra);
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Report(args) => args,
            _ => unreachable!(),
        }
    }

    fn stored(url: &str, private: bool) -> notes::Stored {
        notes::Stored {
            path: "n.md".into(),
            private,
            hidden: false,
            note: notes::parse(&format!("url: {url}\ndate: 2026-11-18\n\nWhy.\n")).unwrap(),
        }
    }

    #[test]
    fn a_shared_report_leaves_out_notes_from_a_private_directory() {
        let listed = "- [o/r#1](https://github.com/o/r/pull/1) x\n";
        let found = vec![
            stored("https://github.com/o/r/pull/1", false),
            stored("https://github.com/o/r/pull/1", true),
        ];
        let kept = shared_notes(&report_args(&["--notes"]), found.clone(), || listed.into());
        assert_eq!(kept, found[..1]);
        let kept = shared_notes(
            &report_args(&["--notes", "--private"]),
            found.clone(),
            || panic!("a private report shows every note"),
        );
        assert_eq!(kept, found);
        // Only private notes: the report is not read for them.
        let kept = shared_notes(&report_args(&["--notes"]), found[1..].to_vec(), || {
            panic!("no public note to place")
        });
        assert!(kept.is_empty());
    }

    #[test]
    fn a_report_hides_embargoed_notes_unless_asked() {
        let mut held = stored("https://github.com/o/secret/pull/1", true);
        held.note.headers = vec![("embargo".into(), "until disclosed".into())];
        let found = vec![held, stored("https://github.com/o/r/pull/1", false)];
        let no_listing = || -> String { panic!("a private report shows every note") };
        let carried = report_notes(
            &report_args(&["--notes", "--private"]),
            found.clone(),
            no_listing,
        );
        assert!(carried[0].hidden && !carried[1].hidden);
        assert_eq!(carried[0].note.text, notes::HIDDEN_TEXT);
        let carried = report_notes(
            &report_args(&["--notes", "--private", "--show-embargoed"]),
            found,
            no_listing,
        );
        assert!(!carried[0].hidden);
        assert_eq!(carried[0].note.text, "Why.");
    }

    #[test]
    fn show_embargoed_needs_a_private_report() {
        use clap::Parser;
        let argv = [
            "sandogasa-report",
            "report",
            "-d",
            "x",
            "--period",
            "2026H2",
        ];
        let with =
            |extra: &[&str]| Cli::try_parse_from(argv.iter().chain(extra).copied()).map(|_| ());
        assert!(with(&["--notes", "--show-embargoed"]).is_err());
        assert!(with(&["--notes", "--private", "--show-embargoed"]).is_ok());
    }

    #[test]
    fn release_lists_notes_by_their_files_unless_asked() {
        let mut s = stored("https://github.com/o/secret/pull/1", true);
        s.path = "/e/2026/2026-11-18T1032-box.md".into();
        let list = release_list(std::slice::from_ref(&s), false);
        assert_eq!(list, "2026-11-18 /e/2026/2026-11-18T1032-box.md\n");
        assert!(release_list(&[s], true).contains("o/secret"));
    }

    #[test]
    fn rewrite_points_notes_on_moved_repositories_at_their_new_home() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = [notes::NoteDir {
            path: dir.path().into(),
            private: false,
        }];
        let year = dir.path().join("2026");
        std::fs::create_dir_all(&year).unwrap();
        let write = |name: &str, url: &str| {
            let path = year.join(name);
            std::fs::write(&path, format!("url: {url}\ndate: 2026-11-18\n\nWhy.\n")).unwrap();
            path
        };
        let moved = write("a.md", "https://github.com/old/r/commit/abc");
        // On GitLab, so GitHub is not asked about it.
        let other = write("b.md", "https://gitlab.com/o/r/-/issues/1");
        let cfg: config::ReportConfig =
            toml::from_str("[notes.moved]\n\"github.com/old/r\" = \"github.com/new/r\"\n").unwrap();
        let (found, _) = notes::load_dirs(&dirs);
        rewrite_notes(&cfg, found, true, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&moved).unwrap(),
            "url: https://github.com/new/r/commit/abc\ndate: 2026-11-18\n\nWhy.\n"
        );
        assert!(
            std::fs::read_to_string(&other)
                .unwrap()
                .contains("gitlab.com/o/r")
        );
        // Nothing left to rewrite.
        let (found, _) = notes::load_dirs(&dirs);
        rewrite_notes(&cfg, found, true, false).unwrap();

        // A note that cannot be rewritten does not keep the rest.
        let gone = write("c.md", "https://github.com/old/r/pull/1");
        let next = write("d.md", "https://github.com/old/r/pull/2");
        let (found, _) = notes::load_dirs(&dirs);
        std::fs::remove_file(&gone).unwrap();
        let err = rewrite_notes(&cfg, found, true, false).unwrap_err();
        assert_eq!(err, "1 note(s) could not be rewritten");
        assert!(
            std::fs::read_to_string(&next)
                .unwrap()
                .contains("github.com/new/r/pull/2")
        );
    }

    #[test]
    fn release_moves_only_lifted_notes_from_private_directories() {
        let public = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        let dirs = [
            notes::NoteDir {
                path: public.path().into(),
                private: false,
            },
            notes::NoteDir {
                path: private.path().into(),
                private: true,
            },
        ];
        let write = |name: &str, embargo: &str| {
            let year = private.path().join("2026");
            std::fs::create_dir_all(&year).unwrap();
            let path = year.join(name);
            std::fs::write(
                &path,
                format!("date: 2026-11-18\nembargo: {embargo}\n\n{name}\n"),
            )
            .unwrap();
            path
        };
        let lifted = write("a.md", "lifted 2026-11-19");
        let held = write("b.md", "until disclosed");
        let (found, _) = notes::load_dirs(&dirs);
        release_notes(&dirs, found, true, false).unwrap();
        assert!(!lifted.exists());
        assert!(public.path().join("2026/a.md").exists());
        assert!(held.exists());

        // One that cannot be moved does not keep the rest.
        let gone = write("d.md", "lifted 2026-11-19");
        let next = write("e.md", "lifted 2026-11-19");
        let (found, _) = notes::load_dirs(&dirs);
        std::fs::remove_file(&gone).unwrap();
        let err = release_notes(&dirs, found, true, false).unwrap_err();
        assert!(err.starts_with("1 of 2 note(s)"), "{err}");
        assert!(!next.exists());
        assert!(public.path().join("2026/e.md").exists());
        assert!(!public.path().join("2026/d.md").exists());

        // Nothing left to move; no public directory to move to.
        let (found, _) = notes::load_dirs(&dirs);
        release_notes(&dirs, found, true, false).unwrap();
        write("c.md", "lifted 2026-11-19");
        let (found, _) = notes::load_dirs(&dirs[1..]);
        let err = release_notes(&dirs[1..], found, true, false).unwrap_err();
        assert!(err.contains("no public directory"), "{err}");
    }

    #[test]
    fn unattached_notes_follow_the_flag_then_the_config() {
        let args = report_args;
        let no = |_: &str| -> bool { panic!("no question expected") };
        let plain = args(&["--notes"]);
        // Unset everywhere: included.
        assert!(include_unattached(&plain, None, 3, false, no));
        assert!(!include_unattached(
            &plain,
            Some(notes::Unattached::Exclude),
            3,
            false,
            no
        ));
        // The flag wins over the config.
        let flagged = args(&["--notes", "--unattached-notes", "include"]);
        assert!(include_unattached(
            &flagged,
            Some(notes::Unattached::Exclude),
            3,
            false,
            no
        ));
        let excluded = args(&["--notes", "--unattached-notes", "exclude"]);
        assert!(!include_unattached(&excluded, None, 3, false, no));
        let config: config::NotesConfig = toml::from_str("unattached = \"ask\"").unwrap();
        assert_eq!(config.unattached, Some(notes::Unattached::Ask));
        assert!(toml::from_str::<config::NotesConfig>("unattached = \"maybe\"").is_err());
        // ask: asked at a terminal, the answer kept; yes without one.
        let ask = Some(notes::Unattached::Ask);
        assert!(include_unattached(&plain, ask, 3, false, no));
        assert!(!include_unattached(&plain, ask, 3, true, |q: &str| {
            assert_eq!(q, "Add the 3 note(s) on items not in this report?");
            false
        }));
        // Only with --notes.
        let argv = [
            "sandogasa-report",
            "report",
            "-d",
            "x",
            "--period",
            "2026H2",
            "--unattached-notes",
            "ask",
        ];
        assert!(Cli::try_parse_from(argv).is_err());
    }

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
            no_bts: false,
            no_bugzilla: false,
            no_bodhi: false,
            no_koji: false,
            no_launchpad: false,
            no_gitlab: false,
            no_pagure: false,
            no_github: false,
            no_forgejo: false,
            no_sourcehut: false,
            detailed: 0,
            json: false,
            notes: false,
            show_embargoed: false,
            unattached_notes: None,
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
            no_bts: false,
            no_bugzilla: false,
            no_bodhi: false,
            no_koji: false,
            no_launchpad: false,
            no_gitlab: false,
            no_pagure: false,
            no_github: false,
            no_forgejo: false,
            no_sourcehut: false,
            detailed: 0,
            json: false,
            notes: false,
            show_embargoed: false,
            unattached_notes: None,
            output: None,
            private: false,
            verbose: false,
        };
        let (s, e) = resolve_date_range(&cli).unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
    }
}
