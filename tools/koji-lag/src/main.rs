// SPDX-License-Identifier: Apache-2.0 OR MIT

//! koji-lag CLI: fetch, merge, and report on Koji build lag.

use std::collections::BTreeSet;
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
    /// Sweep a Koji completion window into a local dataset.
    Fetch(FetchArgs),
    /// Union datasets collected independently into one.
    Merge(MergeArgs),
    /// Per-arch queue-wait / build-time / bottleneck report.
    Report(ReportArgs),
}

#[derive(clap::Args)]
struct FetchArgs {
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

    /// Sweep the last N complete UTC days.
    #[arg(long, value_name = "N")]
    days: Option<u32>,

    /// Keep only builds submitted by this user.
    #[arg(long, value_name = "NAME")]
    owner: Option<String>,

    /// Keep only these source packages (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "NAME,...")]
    package: Vec<String>,

    /// Keep only packages from these inventory files.
    #[arg(short, long, value_name = "FILE")]
    inventory: Vec<String>,

    /// Dataset file to create or merge into.
    #[arg(short, long, value_name = "FILE")]
    output: PathBuf,

    /// Tasks per listTasks page.
    #[arg(long, default_value_t = 1000)]
    page_size: i64,

    /// Pause between hub requests, in milliseconds.
    #[arg(long, default_value_t = 500)]
    sleep_ms: u64,

    /// Retries per failed hub request.
    #[arg(long, default_value_t = 3)]
    retries: u32,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct MergeArgs {
    /// Dataset files to union.
    #[arg(required = true, value_name = "FILE")]
    inputs: Vec<PathBuf>,

    /// Merged output file.
    #[arg(short, long, value_name = "FILE")]
    output: PathBuf,
}

#[derive(clap::Args)]
struct ReportArgs {
    /// Dataset file(s) to report over (merged in memory).
    #[arg(required = true, value_name = "FILE")]
    inputs: Vec<PathBuf>,

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
    match cli.command {
        Command::Fetch(args) => cmd_fetch(&args),
        Command::Merge(args) => cmd_merge(&args),
        Command::Report(args) => cmd_report(&args),
    }
}

fn cmd_fetch(args: &FetchArgs) -> ExitCode {
    let (instance_key, hub_url) = match instance::resolve(&args.instance, args.hub_url.as_deref()) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Freeze the bounds once; whole-UTC-day semantics live in
    // fetch::resolve_window.
    let now = Utc::now().timestamp() as f64;
    let (after, before) =
        match fetch::resolve_window(args.since.as_deref(), args.until.as_deref(), args.days, now) {
            Ok(window) => window,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };

    let mut packages: Option<BTreeSet<String>> = None;
    if !args.package.is_empty() {
        packages = Some(args.package.iter().cloned().collect());
    }
    if !args.inventory.is_empty() {
        let inventory = match sandogasa_inventory::load_and_merge(&args.inventory) {
            Ok(inv) => inv,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let set = packages.get_or_insert_with(BTreeSet::new);
        set.extend(inventory.package.iter().map(|p| p.name.clone()));
    }

    let opts = fetch::FetchOpts {
        instance_key,
        hub_url,
        after,
        before,
        owner: args.owner.clone(),
        packages,
        page_size: args.page_size,
        sleep_ms: args.sleep_ms,
        retries: args.retries,
        verbose: args.verbose,
    };
    match fetch::run(&opts, &args.output) {
        Ok(report) => {
            eprintln!(
                "swept {} build(s), {} buildArch task(s); {} added, \
                 {} refreshed -> {}",
                report.builds_swept,
                report.tasks_swept,
                report.records_added,
                report.records_replaced,
                args.output.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_merge(args: &MergeArgs) -> ExitCode {
    let mut merged = Dataset::new();
    for input in &args.inputs {
        let ds = match Dataset::load(input) {
            Ok(ds) => ds,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let stats = merged.merge(ds);
        eprintln!(
            "{}: {} added, {} refreshed, {} unchanged",
            input.display(),
            stats.added,
            stats.replaced,
            stats.unchanged
        );
    }
    for (instance, from, to) in merged.coverage_gaps() {
        eprintln!(
            "warning: coverage gap on {instance}: no data between \
             unix {from:.0} and {to:.0}"
        );
    }
    if merged.mixes_filtered_windows() {
        eprintln!(
            "warning: merged dataset mixes scoped and full fetches — \
             counts under-represent the full instance"
        );
    }
    match merged.save(&args.output) {
        Ok(()) => {
            eprintln!(
                "merged {} file(s) -> {} ({} task(s), {} build(s))",
                args.inputs.len(),
                args.output.display(),
                merged.tasks.len(),
                merged.builds.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_report(args: &ReportArgs) -> ExitCode {
    let mut dataset = Dataset::new();
    for input in &args.inputs {
        match Dataset::load(input) {
            Ok(ds) => {
                dataset.merge(ds);
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
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
            match fetch::date_to_ts(date) {
                Ok(ts) => *target = Some(ts),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    // Inclusive end date.
    if let Some(until) = &mut opts.until {
        *until += 86_400.0;
    }

    let output = report::run(&dataset, &opts);
    if args.json {
        match serde_json::to_string_pretty(&output) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{}", report::render(&output, args.min_samples));
    }
    ExitCode::SUCCESS
}
