// SPDX-License-Identifier: Apache-2.0 OR MIT

//! koji-lag CLI: fetch, merge, and report on Koji build lag.

use std::collections::BTreeSet;
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
    /// Sweep a long window a day at a time, collating as it goes.
    Backfill(BackfillArgs),
    /// Sweep a Koji completion window into a local dataset.
    Fetch(FetchArgs),
    /// Union datasets collected independently into one.
    Merge(MergeArgs),
    /// Per-arch queue-wait / build-time / bottleneck report.
    Report(ReportArgs),
    /// Render reports for every dataset in a tree, without fetching.
    Reports(ReportsArgs),
}

#[derive(clap::Args)]
struct BackfillArgs {
    /// Known Koji instance (cbs, fedora, stream). Names the files.
    #[arg(long, default_value = "fedora")]
    instance: String,

    /// Explicit hub URL (overrides --instance; https only).
    #[arg(long, value_name = "URL")]
    hub_url: Option<String>,

    /// Window start date (UTC midnight, inclusive).
    #[arg(long, value_name = "YYYY-MM-DD", conflicts_with = "days")]
    since: Option<String>,

    /// Window end date, inclusive.
    #[arg(long, value_name = "YYYY-MM-DD")]
    until: Option<String>,

    /// Sweep the last N complete UTC days.
    #[arg(long, value_name = "N")]
    days: Option<u32>,

    /// Directory to write the dataset tree into.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,

    /// Also write reports, into this directory tree.
    #[arg(long, value_name = "DIR")]
    reports_root: Option<PathBuf>,

    /// What to do when a day's dataset is already there.
    #[arg(long, value_name = "WHAT", value_enum, default_value = "ask")]
    if_exists: koji_lag::backfill::Existing,

    /// Tasks per listTasks page.
    #[arg(long, default_value_t = 1000)]
    page_size: i64,

    /// Minimum pause between hub requests, in milliseconds.
    #[arg(long, default_value_t = 500)]
    sleep_ms: u64,

    /// Share of one connection to use, as a percentage.
    #[arg(long, value_name = "PERCENT", default_value_t = 50)]
    duty_cycle: u32,

    /// Retries per failed hub request.
    #[arg(long, default_value_t = 3)]
    retries: u32,

    /// Withhold report stats below this sample count.
    #[arg(long, default_value_t = 5)]
    min_samples: usize,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
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

    /// Start below this build task id, or below the oldest in a
    /// dataset file.
    ///
    /// A backfill knows the answer: a dataset covering a later
    /// window holds nothing this one wants, so the sweep can begin
    /// below the oldest build in it. Given a number, that id is
    /// used directly.
    #[arg(long, value_name = "ID|FILE")]
    start_below: Option<String>,
}

#[derive(clap::Args)]
struct ReportsArgs {
    /// Dataset tree to read (as written by `backfill`).
    #[arg(long, value_name = "DIR")]
    root: PathBuf,

    /// Directory tree to write reports into.
    #[arg(long, value_name = "DIR")]
    reports_root: PathBuf,

    /// Withhold report stats below this sample count.
    #[arg(long, default_value_t = 5)]
    min_samples: usize,

    /// Re-render reports that already exist.
    ///
    /// Off by default, so re-running after an interruption costs
    /// nothing; pass it after changing what a report says.
    #[arg(long)]
    force: bool,
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
        Command::Backfill(args) => cmd_backfill(&args),
        Command::Fetch(args) => cmd_fetch(&args),
        Command::Merge(args) => cmd_merge(&args),
        Command::Report(args) => cmd_report(&args),
        Command::Reports(args) => cmd_reports(&args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_fetch(args: &FetchArgs) -> Result<(), Box<dyn Error>> {
    let (instance_key, hub_url) = instance::resolve(&args.instance, args.hub_url.as_deref())?;
    // Freeze the bounds once; whole-UTC-day semantics live in
    // fetch::resolve_window.
    let now = Utc::now().timestamp() as f64;
    let (after, before) =
        fetch::resolve_window(args.since.as_deref(), args.until.as_deref(), args.days, now)?;

    let mut packages: Option<BTreeSet<String>> = None;
    if !args.package.is_empty() {
        packages = Some(args.package.iter().cloned().collect());
    }
    if !args.inventory.is_empty() {
        let inventory = sandogasa_inventory::load_and_merge(&args.inventory)?;
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
        duty_percent: args.duty_cycle,
        verbose: args.verbose,
        start_below: match args.start_below.as_deref() {
            None => None,
            Some(spec) => Some(oldest_build_id(spec)?),
        },
    };
    let report = fetch::run(&opts, &args.output)?;
    eprintln!(
        "swept {} build(s), {} child task(s); {} added, \
         {} refreshed -> {}",
        report.builds_swept,
        report.tasks_swept,
        report.records_added,
        report.records_replaced,
        args.output.display()
    );
    Ok(())
}

/// Resolve `--start-below`: a build task id, or the oldest one in a
/// dataset.
///
/// Taking a file is the useful form during a backfill — the answer is
/// already sitting in the dataset for the window after this one, and
/// reading it beats copying an id by hand.
fn oldest_build_id(spec: &str) -> Result<i64, Box<dyn Error>> {
    if let Ok(id) = spec.trim().parse::<i64>() {
        return Ok(id);
    }
    let dataset = koji_lag::dataset::Dataset::load(std::path::Path::new(spec))
        .map_err(|e| format!("--start-below {spec}: {e}"))?;
    dataset
        .builds
        .values()
        .map(|b| b.task_id)
        .min()
        .ok_or_else(|| format!("--start-below {spec}: the dataset holds no builds").into())
}

/// Sweep a window a day at a time, collating finished periods.
///
/// Days are swept newest-first so each one can bound the next: the oldest
/// build of the day just written is a ceiling for the day before it, which
/// is what keeps a sixty-day backfill from walking the same history sixty
/// times.
fn cmd_backfill(args: &BackfillArgs) -> Result<(), Box<dyn Error>> {
    use chrono::{Duration, NaiveDate};
    use koji_lag::backfill::{
        Existing, Grain, collate, complete, month_of, week_of, weeks_of_month,
    };

    let (instance_key, hub_url) = instance::resolve(&args.instance, args.hub_url.as_deref())?;
    // The file is named for the instance, so a pooled tree can hold
    // Fedora's and CentOS's sweeps side by side without collision.
    let file = format!("{instance_key}.json");
    let now = Utc::now().timestamp() as f64;
    let (after, before) =
        fetch::resolve_window(args.since.as_deref(), args.until.as_deref(), args.days, now)?;
    let day_of = |ts: f64| -> Result<NaiveDate, String> {
        chrono::DateTime::from_timestamp(ts as i64, 0)
            .map(|d| d.date_naive())
            .ok_or_else(|| format!("cannot read {ts} as a date"))
    };
    let (first, last) = (day_of(after)?, day_of(before - 1.0)?);

    let mut bound: Option<i64> = None;
    let mut day = last;
    let mut swept = 0usize;
    while day >= first {
        let dir = args.root.join(Grain::Daily.path(day));
        // Before the sweep, so a tree that cannot be written fails in
        // seconds rather than after the day's requests.
        std::fs::create_dir_all(&dir)?;
        let out = dir.join(&file);
        let mut skip = false;
        if out.exists() {
            match args.if_exists {
                Existing::Replace => std::fs::remove_file(&out)?,
                Existing::Merge => {}
                // Nobody to ask in a cron job, and merging cannot lose
                // what is already there, so that is what an unattended
                // run does.
                Existing::Ask => {
                    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                        let again = sandogasa_cli::confirm(
                            &format!("{} exists; sweep it again and merge?", out.display()),
                            true,
                        )?;
                        skip = !again;
                    } else {
                        eprintln!("note: {} exists; merging into it", out.display());
                    }
                }
            }
        }
        if !skip {
            let day_start = day.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc();
            let opts = fetch::FetchOpts {
                instance_key: instance_key.clone(),
                hub_url: hub_url.clone(),
                after: day_start.timestamp() as f64,
                before: (day_start + Duration::days(1)).timestamp() as f64,
                owner: None,
                packages: None,
                page_size: args.page_size,
                sleep_ms: args.sleep_ms,
                retries: args.retries,
                duty_percent: args.duty_cycle,
                verbose: args.verbose,
                start_below: bound,
            };
            eprintln!("[koji-lag] backfill: {day}");
            let report = fetch::run(&opts, &out)?;
            eprintln!(
                "  {} build(s), {} task(s) -> {}",
                report.builds_swept,
                report.tasks_swept,
                out.display()
            );
            swept += 1;
        }
        // Whether swept or skipped, the day on disk bounds the next one.
        bound = oldest_build_id(&out.display().to_string()).ok();
        report_for(args, &Grain::Daily.path(day), std::slice::from_ref(&out))?;

        // A day completing its week (and a week completing its month)
        // happens on the oldest day of that period, which is the one just
        // done, since the sweep runs backwards.
        let week = week_of(day);
        if complete(&args.root, &week, &file, Grain::Daily) {
            let parts: Vec<_> = week.days().map(|d| Grain::Daily.path(d)).collect();
            let n = collate(&args.root, &week, Grain::Daily, &parts, &file)?;
            eprintln!("  collated {n} day(s) into {}", week.path().display());
            report_for(
                args,
                &week.path(),
                &[args.root.join(week.path()).join(&file)],
            )?;
        }
        let month = month_of(day);
        let weeks = weeks_of_month(day);
        if weeks
            .iter()
            .all(|w| args.root.join(w.path()).join(&file).exists())
        {
            let parts: Vec<_> = weeks.iter().map(|w| w.path()).collect();
            let n = collate(&args.root, &month, Grain::Weekly, &parts, &file)?;
            eprintln!("  collated {n} week(s) into {}", month.path().display());
            report_for(
                args,
                &month.path(),
                &[args.root.join(month.path()).join(&file)],
            )?;
        }

        day -= Duration::days(1);
    }
    eprintln!(
        "backfill: swept {swept} day(s) into {}",
        args.root.display()
    );
    Ok(())
}

/// Write a chunk's reports, when a reports root was given.
///
/// Reports are kept at every grain rather than collated away: they are
/// kilobytes against the datasets' megabytes, and a daily report answers
/// questions a monthly one has already averaged out.
fn report_for(
    args: &BackfillArgs,
    relative: &std::path::Path,
    inputs: &[PathBuf],
) -> Result<(), Box<dyn Error>> {
    let Some(root) = &args.reports_root else {
        return Ok(());
    };
    let mut dataset = Dataset::new();
    let mut found = false;
    for input in inputs {
        if input.exists() {
            dataset.merge(Dataset::load(input)?);
            found = true;
        }
    }
    if !found {
        return Ok(());
    }
    let output = report::run(&dataset, &report::ReportOpts::default());
    write_report(&root.join(relative), &output, args.min_samples)?;
    Ok(())
}

/// Render a report beside every dataset in a tree.
///
/// Separate from `backfill` because rendering is cheap and sweeping is
/// not: a change to what a report says should not mean asking Koji for
/// the data again. The tree's shape is the input — whatever
/// `daily/`, `weekly/` and `monthly/` hold gets a report at the matching
/// path.
fn cmd_reports(args: &ReportsArgs) -> Result<(), Box<dyn Error>> {
    let mut rendered = 0usize;
    let mut skipped = 0usize;
    for grain in ["daily", "weekly", "monthly"] {
        let root = args.root.join(grain);
        if !root.exists() {
            continue;
        }
        for dataset_path in datasets_under(&root)? {
            let relative = dataset_path
                .strip_prefix(&args.root)
                .unwrap_or(&dataset_path)
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            let out = args.reports_root.join(&relative);
            if !args.force && out.join("report.txt").exists() && out.join("report.json").exists() {
                skipped += 1;
                continue;
            }
            let dataset = Dataset::load(&dataset_path)?;
            let output = report::run(&dataset, &report::ReportOpts::default());
            write_report(&out, &output, args.min_samples)?;
            rendered += 1;
        }
    }
    eprintln!(
        "reports: {rendered} written, {skipped} already present -> {}",
        args.reports_root.display()
    );
    Ok(())
}

/// Every dataset file under `dir`, depth-first.
fn datasets_under(dir: &std::path::Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json") {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn cmd_merge(args: &MergeArgs) -> Result<(), Box<dyn Error>> {
    let mut merged = Dataset::new();
    for input in &args.inputs {
        let ds = Dataset::load(input)?;
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
    merged.save(&args.output)?;
    eprintln!(
        "merged {} file(s) -> {} ({} task(s), {} build(s))",
        args.inputs.len(),
        args.output.display(),
        merged.tasks.len(),
        merged.builds.len()
    );
    Ok(())
}

fn cmd_report(args: &ReportArgs) -> Result<(), Box<dyn Error>> {
    let mut dataset = Dataset::new();
    for input in &args.inputs {
        dataset.merge(Dataset::load(input)?);
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
            *target = Some(fetch::date_to_ts(date)?);
        }
    }
    // Inclusive end date.
    if let Some(until) = &mut opts.until {
        *until += 86_400.0;
    }

    let output = report::run(&dataset, &opts);
    match &args.out {
        Some(dir) => {
            let written = write_report(dir, &output, args.min_samples)?;
            eprintln!("wrote {}", written.join(", "));
        }
        None if args.json => println!("{}", serde_json::to_string_pretty(&output)?),
        None => print!("{}", report::render(&output, args.min_samples)),
    }
    Ok(())
}

/// Write a report to `dir` in both forms, returning what was written.
///
/// Kept in one place because the tree-walking commands write reports the
/// same way, and a report that exists as text but not as JSON (or the
/// reverse) is the kind of difference nobody notices until a script
/// needs the missing one.
fn write_report(
    dir: &std::path::Path,
    output: &report::ReportOutput,
    min_samples: usize,
) -> Result<Vec<String>, Box<dyn Error>> {
    std::fs::create_dir_all(dir)?;
    let text = dir.join("report.txt");
    let json = dir.join("report.json");
    std::fs::write(&text, report::render(output, min_samples))?;
    std::fs::write(
        &json,
        format!("{}\n", serde_json::to_string_pretty(output)?),
    )?;
    Ok(vec![text.display().to_string(), json.display().to_string()])
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
