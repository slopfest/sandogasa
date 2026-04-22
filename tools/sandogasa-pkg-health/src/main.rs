// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use sandogasa_pkg_health::{Context, HealthReport, duration, registry::default_registry};

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
    /// List registered health checks and their cost tiers.
    Checks,
    /// Run health checks against an inventory.
    Run(RunArgs),
}

#[derive(clap::Args)]
struct RunArgs {
    /// Path to inventory TOML file.
    #[arg(short, long)]
    inventory: String,

    /// Path to report TOML file (read if exists, written after run).
    #[arg(short, long)]
    output: String,

    /// Run only these checks (repeatable).
    #[arg(long = "check", value_name = "ID")]
    checks: Vec<String>,

    /// Run all cheap-tier checks.
    #[arg(long, conflicts_with_all = ["medium", "expensive", "all"])]
    cheap: bool,

    /// Run all medium-tier checks.
    #[arg(long, conflicts_with_all = ["cheap", "expensive", "all"])]
    medium: bool,

    /// Run all expensive-tier checks.
    #[arg(long, conflicts_with_all = ["cheap", "medium", "all"])]
    expensive: bool,

    /// Run all checks regardless of tier.
    #[arg(long, conflicts_with_all = ["cheap", "medium", "expensive"])]
    all: bool,

    /// Re-run any selected check whose stored result is older than
    /// this duration (e.g. "7d", "24h").
    #[arg(long, value_name = "DURATION")]
    max_age: Option<String>,

    /// Limit to specific packages (repeatable).
    #[arg(long = "package", value_name = "NAME")]
    packages: Vec<String>,

    /// Output summary as JSON.
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to build runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        match cli.command {
            Command::Checks => cmd_checks(),
            Command::Run(args) => cmd_run(&args),
        }
    })
}

fn cmd_checks() -> ExitCode {
    let reg = default_registry();
    println!("Available health checks:\n");
    for check in reg.all() {
        println!(
            "  {:20} [{:?}] — {}",
            check.id(),
            check.cost_tier(),
            check.description()
        );
    }
    ExitCode::SUCCESS
}

fn cmd_run(args: &RunArgs) -> ExitCode {
    let inventory = match sandogasa_inventory::load(&args.inventory) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let reg = default_registry();

    // Load existing report or create fresh.
    let mut report = if std::path::Path::new(&args.output).exists() {
        match HealthReport::load(&args.output) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        HealthReport::new(&inventory.inventory.name)
    };

    // Determine which checks to run.
    let selected_ids: Vec<&str> = if args.all {
        reg.all().map(|c| c.id()).collect()
    } else if args.cheap {
        reg.by_tier(sandogasa_pkg_health::CostTier::Cheap)
            .map(|c| c.id())
            .collect()
    } else if args.medium {
        reg.by_tier(sandogasa_pkg_health::CostTier::Medium)
            .map(|c| c.id())
            .collect()
    } else if args.expensive {
        reg.by_tier(sandogasa_pkg_health::CostTier::Expensive)
            .map(|c| c.id())
            .collect()
    } else if !args.checks.is_empty() {
        args.checks.iter().map(|s| s.as_str()).collect()
    } else {
        // Default: all cheap checks.
        reg.by_tier(sandogasa_pkg_health::CostTier::Cheap)
            .map(|c| c.id())
            .collect()
    };

    if args.verbose {
        eprintln!("[pkg-health] running checks: {}", selected_ids.join(", "));
    }

    // Determine which packages to check.
    let packages: Vec<&str> = if args.packages.is_empty() {
        inventory.package.iter().map(|p| p.name.as_str()).collect()
    } else {
        args.packages.iter().map(|s| s.as_str()).collect()
    };

    if args.verbose {
        eprintln!("[pkg-health] {} package(s) to check", packages.len());
    }

    // Parse --max-age if given.
    let max_age = match args.max_age.as_deref() {
        Some(s) => match duration::parse(s) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let ctx = Context::new();
    let mut ran = 0;
    let mut fresh = 0;
    let mut failed = 0;
    let total = packages.len();
    let width = total.to_string().len();

    for (i, pkg) in packages.iter().enumerate() {
        if args.verbose {
            eprintln!(
                "[pkg-health] [{:>width$}/{total}] {pkg}",
                i + 1,
                width = width,
            );
        }
        for check_id in &selected_ids {
            let Some(check) = reg.get(check_id) else {
                eprintln!("warning: unknown check '{check_id}'");
                continue;
            };

            // --max-age: skip if stored result is still fresh.
            if let Some(age) = max_age
                && !report.is_stale(pkg, check_id, age)
            {
                fresh += 1;
                continue;
            }

            match check.run(pkg, &ctx) {
                Ok(result) => {
                    report.update(pkg, check_id, result.data);
                    ran += 1;
                }
                Err(e) => {
                    eprintln!("warning: {pkg}: {check_id}: {e}");
                    failed += 1;
                }
            }
        }
    }

    if let Err(e) = report.save(&args.output) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("JSON serialization failed")
        );
    } else {
        eprintln!(
            "Ran {ran} check(s), {fresh} fresh (skipped), {failed} failed, wrote report to {}",
            args.output
        );
    }

    ExitCode::SUCCESS
}
