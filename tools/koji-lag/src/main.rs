// SPDX-License-Identifier: Apache-2.0 OR MIT

//! koji-lag CLI: fetch, merge, and report on Koji build lag.

use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

use chrono::Utc;
use clap::{Parser, Subcommand};
use koji_lag::dataset::Dataset;
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
    /// Write a store's rows out as CSV, for analysis elsewhere.
    Export(ExportArgs),
    /// Read JSON datasets into the store.
    ///
    /// Transitional and hidden: it exists to fold datasets collected
    /// before the store into one, and is to be removed once that is done —
    /// no release should document it. See TODO.md.
    #[command(hide = true)]
    Import(ImportArgs),
    /// Per-arch queue-wait / build-time / bottleneck report.
    Report(ReportArgs),
    /// Write reports for every period the store covers.
    Reports(ReportsArgs),
    /// Fetch whatever the store is missing for a window.
    Sync(SyncArgs),
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
struct ImportArgs {
    /// JSON dataset file, or a tree of them.
    #[arg(required = true, value_name = "PATH")]
    inputs: Vec<PathBuf>,

    /// Store to read into (created if absent).
    #[arg(long, value_name = "FILE")]
    store: PathBuf,
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

    /// Tasks per listTasks page.
    #[arg(long, default_value_t = 1000)]
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

    /// Print each period as it is considered.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct ReportArgs {
    /// Dataset file(s) to report over (merged in memory).
    #[arg(value_name = "FILE", required_unless_present = "store")]
    inputs: Vec<PathBuf>,

    /// Store to report from, instead of dataset files.
    #[arg(long, value_name = "FILE", conflicts_with = "inputs")]
    store: Option<PathBuf>,

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

    /// Output machine-readable JSON instead of tables.
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    let result = match cli.command {
        Command::Report(args) => cmd_report(&args),
        Command::Reports(args) => cmd_reports(&args),
        Command::Export(args) => cmd_export(&args),
        Command::Import(args) => cmd_import(&args),
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

fn cmd_import(args: &ImportArgs) -> Result<(), Box<dyn Error>> {
    let mut store = koji_lag::store::Store::open(&args.store)?;
    let mut total = koji_lag::import::Imported::default();
    for input in &args.inputs {
        let one = koji_lag::import::ingest_path(&mut store, input)?;
        total.written.builds += one.written.builds;
        total.written.tasks += one.written.tasks;
        total.children_current += one.children_current;
        total.children_behind += one.children_behind;
    }
    println!(
        "imported {} build(s), {} task(s) into {}",
        total.written.builds,
        total.written.tasks,
        args.store.display()
    );
    if total.children_behind > 0 {
        println!(
            "note: {} build(s) came from a dataset without the SRPM stage, \
             recorded as an older generation; a sweep will ask for their \
             children again",
            total.children_behind
        );
    }
    for (instance, counts) in store.counts()? {
        println!(
            "  {instance}: {} build(s), {} task(s) stored",
            counts.builds, counts.tasks
        );
    }
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
        "reports: {} written, {} already present, {} not complete in the store -> {}",
        // Two files per period, which is not what a reader means by a
        // report count.
        pooled.written.len() / 2,
        pooled.present,
        pooled.incomplete,
        args.reports_root.display()
    );
    Ok(())
}

fn cmd_report(args: &ReportArgs) -> Result<(), Box<dyn Error>> {
    let mut opts = report::ReportOpts {
        arches: args.arch.clone(),
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
    // Loading a whole store to filter it down in memory would defeat the
    // point of having one.
    let dataset = match &args.store {
        Some(path) => {
            let (instance_key, _) = instance::resolve(&args.instance, None)?;
            let store = koji_lag::store::Store::open(path)?;
            let (from, to) = (opts.since.unwrap_or(0.0), opts.until.unwrap_or(f64::MAX));
            // Whole days only, as everywhere else: statistics over a day
            // whose arch tasks have not arrived read as a quiet day rather
            // than an unfinished one.
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
            // The store applied the window already, selecting a build's
            // children by the build rather than by their own clocks.
            // Applying it again here would drop the arch tasks of a build
            // that finished just before midnight and split it across two
            // periods — the thing the store query exists to avoid. The
            // period moves to `period`, which is what the report states it
            // covers and judges its coverage against.
            opts.period = Some((from, to));
            opts.since = None;
            opts.until = None;
            selection.dataset
        }
        None => {
            let mut dataset = Dataset::new();
            for input in &args.inputs {
                dataset.merge(Dataset::load(input)?);
            }
            dataset
        }
    };

    let output = report::run(&dataset, &opts);
    match &args.out {
        Some(dir) => {
            let written = koji_lag::pool::write(dir, &output, args.min_samples)?;
            let names: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
            eprintln!("wrote {}", names.join(", "));
        }
        None if args.json => println!("{}", serde_json::to_string_pretty(&output)?),
        None => print!("{}", report::render(&output, args.min_samples)),
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
