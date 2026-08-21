// SPDX-License-Identifier: Apache-2.0 OR MIT

//! koji-lag CLI: fetch, merge, and report on Koji build lag.

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use chrono::Utc;
use clap::{Parser, Subcommand};
use koji_lag::pool::Format;
use koji_lag::{fetch, instance, report};

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    before_help = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"))
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report the mass rebuilds and architecture stalls in a store.
    Events(EventsArgs),
    /// Write a store's rows out as CSV, for analysis elsewhere.
    Export(ExportArgs),
    /// Time one listing page, to size a backfill before starting it.
    Probe(ProbeArgs),
    /// Per-arch queue-wait / build-time / bottleneck report.
    Report(ReportArgs),
    /// Write reports for every period the store covers.
    Reports(ReportsArgs),
    /// Fetch whatever the store is missing for a window.
    Sync(SyncArgs),
}

#[derive(clap::Args)]
struct EventsArgs {
    /// Store to read from.
    #[arg(long, value_name = "FILE")]
    store: PathBuf,

    /// Known Koji instance (cbs, fedora, stream).
    #[arg(long, default_value = "fedora")]
    instance: String,

    /// Directory to write the events tree into.
    #[arg(short, long, value_name = "DIR")]
    out: PathBuf,

    /// First day to consider (default: everything the store holds).
    #[arg(long, value_name = "YYYY-MM-DD")]
    since: Option<String>,

    /// Last day to consider, inclusive.
    #[arg(long, value_name = "YYYY-MM-DD")]
    until: Option<String>,

    /// Release schedule checkout, for announced dates.
    ///
    /// One `f-NN/Fedora.Schedule.xml` per release. With it, a rebuild
    /// reports the dates it was announced for beside the ones it ran
    /// on, and every event names the release cycle it fell in.
    #[arg(long, value_name = "DIR")]
    schedule: Option<PathBuf>,

    /// Extra outage causes, merged with the built-in ones.
    ///
    /// Same form as the tool's own `data/outages.toml`. An entry that
    /// matches no detected event is reported rather than ignored.
    #[arg(long, value_name = "FILE")]
    annotations: Option<PathBuf>,

    /// Withhold report stats below this sample count.
    #[arg(long, default_value_t = 5)]
    min_samples: usize,

    /// Output forms: text, json, csv (comma-separated or repeated).
    #[arg(
        long,
        value_name = "FORM,...",
        value_delimiter = ',',
        hide_possible_values = true
    )]
    format: Vec<koji_lag::pool::Format>,

    /// Name each event as it is written.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct ExportArgs {
    /// Store to read from.
    #[arg(long, value_name = "FILE")]
    store: PathBuf,

    /// Known Koji instance (cbs, fedora, stream).
    #[arg(long, default_value = "fedora")]
    instance: String,

    /// Directory to write the CSV files into.
    #[arg(short, long, value_name = "DIR")]
    out: PathBuf,

    /// First day to export (default: everything the store holds).
    #[arg(long, value_name = "YYYY-MM-DD")]
    since: Option<String>,

    /// Last day to export, inclusive.
    #[arg(long, value_name = "YYYY-MM-DD")]
    until: Option<String>,
}

#[derive(clap::Args)]
struct SyncArgs {
    /// Known Koji instance (cbs, fedora, stream).
    #[arg(long, default_value = "fedora")]
    instance: String,

    /// Explicit hub URL (overrides --instance; https only).
    #[arg(long, value_name = "URL")]
    hub_url: Option<String>,

    /// Window start date (UTC midnight, inclusive).
    #[arg(long, value_name = "YYYY-MM-DD", conflicts_with = "days")]
    since: Option<String>,

    /// Window end date, inclusive (default: the last complete
    /// UTC day — the running day is never included implicitly).
    #[arg(long, value_name = "YYYY-MM-DD")]
    until: Option<String>,

    /// Sync the last N complete UTC days.
    #[arg(long, value_name = "N")]
    days: Option<u32>,

    /// Store to fill (created if absent).
    #[arg(long, value_name = "FILE")]
    store: PathBuf,

    /// Seconds a single hub request may take; 0 waits forever.
    ///
    /// Defaults to $SANDOGASA_KOJI_TIMEOUT, else 600. Raise it for a
    /// deep window: a page that exceeds the bound is abandoned and the
    /// retry pays the hub cost again, so too low a value can stop a
    /// backfill progressing rather than merely slow it. `probe` says
    /// what the depth you are asking for actually costs.
    #[arg(long, value_name = "SECS")]
    timeout: Option<u64>,

    /// Tasks per listTasks page.
    ///
    /// A page costs what it costs to *find*, not to send: at thirteen
    /// months' depth 1000 rows take 18s and 4000 take 21s. Fewer, larger
    /// pages therefore ask the hub for far fewer expensive seeks, and the
    /// duty cycle keeps our share of it the same either way.
    #[arg(long, default_value_t = 4000)]
    page_size: i64,

    /// Minimum pause between hub requests, in milliseconds.
    #[arg(long, default_value_t = 500)]
    sleep_ms: u64,

    /// Share of one connection to use, as a percentage.
    ///
    /// Each pause is scaled to how long the last request took, so a
    /// hub under load is asked less often and a hub that speeds up
    /// is asked more. 50 means pause as long as the request took;
    /// 100 paces by --sleep-ms alone.
    #[arg(long, value_name = "PERCENT", default_value_t = 50)]
    duty_cycle: u32,

    /// Retries per failed hub request.
    #[arg(long, default_value_t = 3)]
    retries: u32,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct ReportsArgs {
    /// Store to report from.
    #[arg(long, value_name = "FILE")]
    store: PathBuf,

    /// Known Koji instance (cbs, fedora, stream).
    #[arg(long, default_value = "fedora")]
    instance: String,

    /// Directory tree to write reports into.
    #[arg(long, value_name = "DIR")]
    reports_root: PathBuf,

    /// First day to consider (default: everything the store holds).
    #[arg(long, value_name = "YYYY-MM-DD")]
    since: Option<String>,

    /// Last day to consider, inclusive.
    #[arg(long, value_name = "YYYY-MM-DD")]
    until: Option<String>,

    /// Withhold report stats below this sample count.
    #[arg(long, default_value_t = 5)]
    min_samples: usize,

    /// Re-render reports that already exist.
    ///
    /// Off by default, so re-running after an interruption costs
    /// nothing; pass it after changing what a report says, or after a
    /// sync has filled in rows a period was missing.
    #[arg(long)]
    force: bool,

    /// Output forms: text, json, csv (comma-separated or repeated).
    ///
    /// Writing to a directory defaults to text,json. `csv` is one file
    /// per table, because a CSV holds one table where the other two
    /// carry every table for the period together.
    // The three are named in the description, so clap's own list of them
    // would only push the line further past 80 columns.
    #[arg(
        long,
        value_name = "FORM,...",
        value_delimiter = ',',
        hide_possible_values = true
    )]
    format: Vec<koji_lag::pool::Format>,

    /// Print each period as it is considered.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct ProbeArgs {
    /// Known Koji instance (cbs, fedora, stream).
    #[arg(long, default_value = "fedora")]
    instance: String,

    /// Koji hub XML-RPC URL (overrides --instance).
    #[arg(long, value_name = "URL")]
    hub_url: Option<String>,

    /// Depths to time, in days before now (comma-separated).
    ///
    /// Defaults to a recent page and one at twenty months, which is
    /// roughly where cost stops being negligible.
    #[arg(
        long,
        value_name = "DAYS,...",
        value_delimiter = ',',
        default_value = "1,600"
    )]
    depth: Vec<f64>,

    /// Seconds a single hub request may take; 0 waits forever.
    ///
    /// Defaults to $SANDOGASA_KOJI_TIMEOUT, else 600. Raise it for a
    /// deep window: a page that exceeds the bound is abandoned and the
    /// retry pays the hub cost again, so too low a value can stop a
    /// backfill progressing rather than merely slow it. `probe` says
    /// what the depth you are asking for actually costs.
    #[arg(long, value_name = "SECS")]
    timeout: Option<u64>,

    /// Rows per page, as `sync --page-size` would ask for.
    #[arg(long, default_value_t = 4000)]
    page_size: i64,

    /// Pages to walk from each depth.
    ///
    /// At least two, because the first page into a region nobody has
    /// asked about lately costs several times what the ones behind it
    /// do — seven minutes against 55s when January 2025 was collected —
    /// and both numbers are needed to size a backfill.
    #[arg(long, default_value_t = 3)]
    steps: usize,

    /// Name each page as it is timed.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct ReportArgs {
    /// Store to report from.
    #[arg(long, value_name = "FILE")]
    store: PathBuf,

    /// Known Koji instance (cbs, fedora, stream), with --store.
    #[arg(long, default_value = "fedora")]
    instance: String,

    /// Only tasks completing on/after this date (UTC midnight).
    #[arg(long, value_name = "YYYY-MM-DD")]
    since: Option<String>,

    /// Only tasks completing on/before this date (UTC midnight).
    #[arg(long, value_name = "YYYY-MM-DD")]
    until: Option<String>,

    /// Restrict to these arches (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "ARCH,...")]
    arch: Vec<String>,

    /// Restrict to builds by these accounts (CSV or repeated).
    ///
    /// The account exactly as Koji records it, which for a service is a
    /// long name: `koschei/koschei-backend01.rdu3.fedoraproject.org`
    /// rather than `koschei`. Answers "how did my own builds fare"
    /// without anybody having to publish a per-person report.
    #[arg(long, value_delimiter = ',', value_name = "NAME,...")]
    owner: Vec<String>,

    /// Restrict to these source packages (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "NAME,...")]
    package: Vec<String>,

    /// Restrict to these classes of build (CSV or repeated).
    ///
    /// One of mass-rebuild, eln-sync, eln-fix, koschei, ci, service,
    /// hand-scratch, official. Restrict to mass-rebuild before comparing
    /// one period with another: an unrestricted window is mostly koschei,
    /// whose mix moves with whatever it retried.
    #[arg(long = "class", value_delimiter = ',', value_name = "CLASS,...")]
    classes: Vec<String>,

    /// Only scratch builds.
    #[arg(long, conflicts_with = "official")]
    scratch: bool,

    /// Only official (non-scratch) builds.
    #[arg(long)]
    official: bool,

    /// Include FAILED tasks in build-time statistics.
    #[arg(long)]
    include_failed: bool,

    /// Write report.txt and report.json into this directory.
    ///
    /// Both forms in one pass, since a reader wants the table and a
    /// machine wants the fields, and running the report twice to get
    /// both would read the dataset twice. Without it the report goes
    /// to stdout, as text or as JSON with --json.
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// Withhold human-output stats below this sample count.
    #[arg(long, default_value_t = 5)]
    min_samples: usize,

    /// Print JSON instead of tables: shorthand for --format json.
    ///
    /// Every tool here takes --json, so it stays as the conventional way
    /// to ask, and means the same thing.
    #[arg(long, conflicts_with = "format")]
    json: bool,

    /// Output forms: text, json, csv (comma-separated or repeated).
    ///
    /// Writing to a directory defaults to text,json. `csv` is one file
    /// per table, because a CSV holds one table where the other two
    /// carry every table for the period together.
    // The three are named in the description, so clap's own list of them
    // would only push the line further past 80 columns.
    #[arg(
        long,
        value_name = "FORM,...",
        value_delimiter = ',',
        hide_possible_values = true
    )]
    format: Vec<koji_lag::pool::Format>,
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    let result = match cli.command {
        Command::Probe(args) => cmd_probe(&args),
        Command::Report(args) => cmd_report(&args),
        Command::Events(args) => cmd_events(&args),
        Command::Reports(args) => cmd_reports(&args),
        Command::Export(args) => cmd_export(&args),
        Command::Sync(args) => cmd_sync(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_sync(args: &SyncArgs) -> Result<(), Box<dyn Error>> {
    let (instance_key, hub_url) = instance::resolve(&args.instance, args.hub_url.as_deref())?;
    let now = Utc::now().timestamp() as f64;
    let (after, before) =
        fetch::resolve_window(args.since.as_deref(), args.until.as_deref(), args.days, now)?;
    // Opened before the hub is asked anything: an unwritable store must
    // fail now rather than after an hour of sweeping.
    let mut store = koji_lag::store::Store::open(&args.store)?;
    let opts = fetch::FetchOpts {
        instance_key,
        hub_url,
        after,
        before,
        page_size: args.page_size,
        sleep_ms: args.sleep_ms,
        retries: args.retries,
        duty_percent: args.duty_cycle,
        timeout: resolve_timeout(args.timeout),
        verbose: args.verbose,
    };
    let report = koji_lag::sync::run(&mut store, &opts)?;
    eprintln!(
        "synced {} page(s): {} build(s), {} child task(s) of {} build(s) -> {}",
        report.pages,
        report.builds,
        report.tasks,
        report.parents_swept,
        args.store.display()
    );
    Ok(())
}

fn cmd_export(args: &ExportArgs) -> Result<(), Box<dyn Error>> {
    let (instance_key, _) = instance::resolve(&args.instance, None)?;
    let store = koji_lag::store::Store::open(&args.store)?;
    let from = match &args.since {
        Some(date) => fetch::date_to_ts(date)?,
        None => 0.0,
    };
    // Inclusive end date, as everywhere else.
    let to = match &args.until {
        Some(date) => fetch::date_to_ts(date)? + 86_400.0,
        None => f64::MAX,
    };
    let exported = koji_lag::export::run(
        &store,
        &instance_key,
        from,
        to,
        fetch::CREATE_GRACE_SECS,
        &args.out,
    )?;
    eprintln!(
        "exported {} build(s), {} task(s), {} host(s), {} channel(s) -> {}",
        exported.builds,
        exported.tasks,
        exported.hosts,
        exported.channels,
        args.out.display()
    );
    // Whole days only, and which ones were left out: a spreadsheet cannot
    // warn its reader about partial data, so partial days never go in it.
    if exported.days_skipped.is_empty() {
        eprintln!("coverage: {} whole day(s)", exported.days_whole);
    } else {
        eprintln!(
            "coverage: {} whole day(s); left out {} incomplete day(s): {}",
            exported.days_whole,
            exported.days_skipped.len(),
            summarise(&exported.days_skipped),
        );
        eprintln!("sync those days to include them.");
    }
    Ok(())
}

/// A few names, then a count: a month of missing days should not print
/// thirty dates.
fn summarise(days: &[String]) -> String {
    const SHOWN: usize = 5;
    match days.len() {
        0 => String::new(),
        n if n <= SHOWN => days.join(", "),
        n => format!("{}, +{} more", days[..SHOWN].join(", "), n - SHOWN),
    }
}

fn cmd_events(args: &EventsArgs) -> Result<(), Box<dyn Error>> {
    let (instance_key, _) = instance::resolve(&args.instance, None)?;
    let store = koji_lag::store::Store::open(&args.store)?;
    // Default to everything listed, so the interesting windows are found
    // without anybody having to know when they were.
    let days = koji_lag::pool::days_in_store(&store, &instance_key)?;
    let midnight = |d: &chrono::NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc()
            .timestamp() as f64
    };
    let (mut from, mut to) = match (days.first(), days.last()) {
        (Some(a), Some(b)) => (midnight(a), midnight(b) + 86_400.0),
        _ => return Err("the store holds no listed days".into()),
    };
    if let Some(date) = &args.since {
        from = from.max(fetch::date_to_ts(date)?);
    }
    if let Some(date) = &args.until {
        to = to.min(fetch::date_to_ts(date)? + 86_400.0);
    }

    let schedule = match &args.schedule {
        Some(dir) => koji_lag::schedule::events(dir)?,
        None => Vec::new(),
    };
    let mut notes = koji_lag::annotate::builtin()?;
    if let Some(path) = &args.annotations {
        notes.extend(koji_lag::annotate::read(path)?);
    }

    let events = koji_lag::events::assemble(&store, &instance_key, from, to, &schedule, &notes)?;
    let formats = Format::for_files(&args.format);
    let mut files = 0;
    for event in &events {
        files += koji_lag::events::write(&args.out, event)?.len();
        // The window's own numbers, beside the summary of it: per-class
        // figures for a rebuild, per-arch for a stall.
        let dataset = store.dataset_for(
            &instance_key,
            event.from,
            event.to,
            fetch::CREATE_GRACE_SECS,
        )?;
        let output = report::run(
            &dataset,
            &report::ReportOpts {
                period: Some((event.from, event.to)),
                ..Default::default()
            },
        );
        files += koji_lag::pool::write(
            &koji_lag::events::dir(&args.out, event),
            &output,
            args.min_samples,
            &formats,
        )?
        .len();
        if args.verbose {
            eprintln!("[koji-lag] events: {}", event.slug());
        }
    }

    // An annotation matching nothing is a gap in the record or a mistake
    // in the note, and either way silence would hide it.
    for note in koji_lag::events::unmatched(&events, &instance_key, &notes) {
        eprintln!(
            "warning: annotation for {} {} ({} .. {}) matched no event",
            note.instance,
            note.arch.as_deref().unwrap_or("all arches"),
            note.from,
            note.to
        );
    }
    let unexplained = events
        .iter()
        .filter(|e| e.kind == koji_lag::events::Kind::Outage && e.causes.is_empty())
        .count();
    if unexplained > 0 {
        eprintln!(
            "note: {unexplained} outage(s) have no recorded cause; \
             see data/outages.toml"
        );
    }
    eprintln!(
        "events: {} in {} file(s) -> {}",
        events.len(),
        files,
        args.out.join("events").display()
    );
    Ok(())
}

fn cmd_reports(args: &ReportsArgs) -> Result<(), Box<dyn Error>> {
    let (instance_key, _) = instance::resolve(&args.instance, None)?;
    let store = koji_lag::store::Store::open(&args.store)?;
    // Which days to consider. Everything the store has listed, unless
    // narrowed: each candidate still has to be complete to be reported,
    // so a generous list costs a coverage query and no more.
    let mut days = koji_lag::pool::days_in_store(&store, &instance_key)?;
    for (flag, keep) in [(&args.since, true), (&args.until, false)] {
        if let Some(date) = flag {
            let bound = fetch::date_to_ts(date)?;
            days.retain(|d| {
                let ts = d
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight")
                    .and_utc()
                    .timestamp() as f64;
                if keep { ts >= bound } else { ts <= bound }
            });
        }
    }
    let opts = koji_lag::pool::PoolOpts {
        report: report::ReportOpts::default(),
        min_samples: args.min_samples,
        formats: Format::for_files(&args.format),
        force: args.force,
        verbose: args.verbose,
    };
    let pooled = koji_lag::pool::run(
        &store,
        &instance_key,
        &args.reports_root,
        &days,
        fetch::CREATE_GRACE_SECS,
        &opts,
    )?;
    eprintln!(
        "reports: {} period(s) in {} file(s), {} already present, \
         {} not complete in the store -> {}",
        pooled.periods,
        pooled.written.len(),
        pooled.present,
        pooled.incomplete,
        args.reports_root.display()
    );
    for w in &pooled.trend.warnings {
        eprintln!("[koji-lag] trend: {w}");
    }
    Ok(())
}

/// A `--timeout` flag beats the environment, and `0` means unbounded in
/// both — one convention, so a caller who has learned it from
/// `SANDOGASA_KOJI_TIMEOUT` does not have to learn it again here.
fn resolve_timeout(flag: Option<u64>) -> Option<std::time::Duration> {
    match flag {
        Some(0) => None,
        Some(secs) => Some(std::time::Duration::from_secs(secs)),
        None => sandogasa_kojihub::xmlrpc::configured_timeout(),
    }
}

fn cmd_probe(args: &ProbeArgs) -> Result<(), Box<dyn Error>> {
    let (instance_key, hub_url) = instance::resolve(&args.instance, args.hub_url.as_deref())?;
    // No pacing: one page per depth is not a load concern, and pacing
    // would time our own politeness rather than the hub's answer.
    let opts = koji_lag::fetch::FetchOpts {
        instance_key,
        hub_url,
        after: 0.0,
        before: 0.0,
        page_size: args.page_size,
        sleep_ms: 0,
        retries: 0,
        duty_percent: 0,
        timeout: resolve_timeout(args.timeout),
        verbose: args.verbose,
    };
    let hub = sandogasa_kojihub::HubClient::with_timeout(&opts.hub_url, opts.timeout);
    let mut pages = koji_lag::sync::HubPages::new(&hub, &opts);
    let now = Utc::now().timestamp() as f64;
    let samples = koji_lag::probe::run(&mut pages, now, &args.depth, args.steps, args.verbose)?;
    print!("{}", koji_lag::probe::render(&samples, &opts));
    Ok(())
}

fn cmd_report(args: &ReportArgs) -> Result<(), Box<dyn Error>> {
    let mut opts = report::ReportOpts {
        arches: args.arch.clone(),
        owners: args.owner.clone(),
        packages: args.package.clone(),
        classes: args
            .classes
            .iter()
            .map(|s| {
                koji_lag::class::Class::from_slug(s).ok_or_else(|| {
                    format!(
                        "unknown class {s:?}; one of {}",
                        koji_lag::class::Class::all().map(|c| c.slug()).join(", ")
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        include_failed: args.include_failed,
        min_samples: args.min_samples,
        ..Default::default()
    };
    if args.scratch {
        opts.scratch = Some(true);
    } else if args.official {
        opts.scratch = Some(false);
    }
    for (flag, target) in [
        (&args.since, &mut opts.since),
        (&args.until, &mut opts.until),
    ] {
        if let Some(date) = flag {
            *target = Some(fetch::date_to_ts(date)?);
        }
    }
    // Inclusive end date.
    if let Some(until) = &mut opts.until {
        *until += 86_400.0;
    }

    // The window is applied twice over a store and once over files: the
    // store selects rows by it, and the report filters by it either way.
    let (instance_key, _) = instance::resolve(&args.instance, None)?;
    let store = koji_lag::store::Store::open(&args.store)?;
    let (from, to) = (opts.since.unwrap_or(0.0), opts.until.unwrap_or(f64::MAX));
    // Whole days only, as everywhere else: statistics over a day whose arch
    // tasks have not arrived read as a quiet day rather than an unfinished
    // one.
    let selection = store.analysable(&instance_key, from, to, fetch::CREATE_GRACE_SECS)?;
    if selection.whole.is_empty() {
        return Err(koji_lag::export::refuse(&selection).into());
    }
    if !selection.skipped.is_empty() {
        eprintln!(
            "note: {} incomplete day(s) left out of this report: {}",
            selection.skipped.len(),
            summarise(&selection.skipped_dates()),
        );
    }
    // The store applied the window already, selecting a build's children by
    // the build rather than by their own clocks. Applying it again here
    // would drop the arch tasks of a build that finished just before
    // midnight and split it across two periods — the thing the store query
    // exists to avoid. The period moves to `period`, which is what the
    // report states it covers and judges its coverage against.
    opts.period = Some((from, to));
    opts.since = None;
    opts.until = None;
    let dataset = selection.dataset;

    let output = report::run(&dataset, &opts);
    match &args.out {
        Some(dir) => {
            let formats = Format::for_files(&args.format);
            let written = koji_lag::pool::write(dir, &output, args.min_samples, &formats)?;
            // The directory and a count: nine absolute paths on one line is
            // not something anyone reads.
            let names: Vec<&str> = written
                .iter()
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                .collect();
            eprintln!("wrote {} into {}", names.join(", "), dir.display());
        }
        // One form to stdout, since a stream is one file. Anything else
        // needs --out, and says so rather than guessing which to print.
        None => match Format::for_stdout(&args.format, args.json)? {
            Format::Json => println!("{}", serde_json::to_string_pretty(&output)?),
            _ => print!("{}", report::render(&output, args.min_samples)),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/man/koji-lag.1"
        ));
    }
}
