// SPDX-License-Identifier: MPL-2.0

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod check_crate;
mod check_update;
mod dag;
mod resolve;

use resolve::{
    FedrqResolver, ResolveOptions, resolve_closure_with_options, resolve_with_installability,
};

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

#[derive(clap::Args, Clone)]
struct ResolveArgs {
    /// Source RPM names to analyze.
    packages: Vec<String>,

    /// Branch to take packages from (e.g. rawhide).
    #[arg(short, long)]
    source: Option<String>,

    /// Repository class for the source branch (fedrq -r).
    #[arg(long, value_name = "REPO")]
    source_repo: Option<String>,

    /// Branch to port packages to (e.g. epel10).
    #[arg(short, long)]
    target: Option<String>,

    /// Repository class for the target branch (fedrq -r).
    #[arg(long, value_name = "REPO")]
    target_repo: Option<String>,

    /// Output as JSON instead of human-readable text.
    #[arg(long)]
    json: bool,

    /// Output build-order as a Koji chain build string.
    #[arg(long)]
    koji: bool,

    /// Generate a shell script for Copr batch builds.
    #[arg(
        long,
        long_help = "\
Generate a shell script for Copr batch builds.

The script accepts the Copr repo as its first
argument, followed by any extra flags to pass
to copr build-package."
    )]
    copr: bool,

    /// Check that subpackages are installable.
    #[arg(
        long,
        long_help = "\
Check that subpackages are installable.

Verifies that the Requires of every
subpackage in the closure can be satisfied
by the target repo or by other packages
in the closure."
    )]
    check_install: bool,

    /// Exclude packages from installability checks.
    #[arg(
        long,
        value_name = "PKG,...",
        value_delimiter = ',',
        long_help = "\
Exclude packages from installability checks.

Comma-separated list of source packages.
Deps provided by these packages are treated
as satisfied and they will not be pulled into
the closure. Useful for packages like glibc
whose version mismatch between Rawhide and
older releases is expected.

May be passed multiple times."
    )]
    exclude_install: Vec<String>,

    /// Disable auto-exclusion of default packages.
    #[arg(
        long,
        long_help = "\
Disable auto-exclusion of default packages
(e.g. glibc) from installability checks.

By default, packages whose version mismatch
between branches is expected and harmless
are excluded automatically."
    )]
    no_auto_exclude: bool,

    /// Max recursion depth (0 = unlimited).
    #[arg(long, default_value = "0")]
    max_depth: usize,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,

    /// Number of parallel fedrq queries (0 = CPUs).
    #[arg(short = 'j', long, default_value = "0")]
    jobs: usize,

    /// Clear fedrq repo metadata cache before querying.
    #[arg(long)]
    refresh: bool,
}

#[derive(clap::Args, Clone)]
struct CheckUpdateArgs {
    /// Koji side tag, Bodhi update alias, or Bodhi URL.
    input: String,

    /// Branch to check against (e.g. epel9).
    #[arg(short = 'b', long)]
    branch: Option<String>,

    /// Repository class for the branch (fedrq -r).
    #[arg(short = 'r', long, value_name = "REPO")]
    repo: Option<String>,

    /// Override branch for @testing queries.
    #[arg(
        long,
        long_help = "\
Override branch for @testing queries.

Auto-detected for EPEL side tags
(e.g. epel9-build-side-* uses epel9).
Otherwise defaults to --branch."
    )]
    testing_branch: Option<String>,

    /// Koji CLI profile (e.g. cbs for CentOS).
    #[arg(long)]
    koji_profile: Option<String>,

    /// Machine-readable JSON output.
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,

    /// Parallel fedrq queries (0 = CPUs).
    #[arg(short = 'j', long, default_value = "0", hide_default_value = true)]
    jobs: usize,
}

#[derive(clap::Args, Clone)]
struct CheckCrateArgs {
    /// Crate name on crates.io.
    name: String,

    /// Crate version (default: latest).
    version: Option<String>,

    /// Target branch (e.g. epel9, rawhide).
    #[arg(short = 'b', long)]
    branch: String,

    /// Repository class for the branch (fedrq -r).
    #[arg(short = 'r', long, value_name = "REPO")]
    repo: Option<String>,

    /// Expand missing deps transitively.
    #[arg(short = 't', long)]
    transitive: bool,

    /// Include dev dependencies in transitive expansion.
    #[arg(long, requires = "transitive")]
    include_dev: bool,

    /// Include optional dependencies in transitive expansion.
    #[arg(long, requires = "transitive")]
    include_optional: bool,

    /// Include too-old deps in transitive expansion.
    #[arg(long, requires = "transitive")]
    include_too_old: bool,

    /// Exclude crates from transitive expansion.
    #[arg(
        long,
        requires = "transitive",
        value_delimiter = ',',
        value_name = "CRATE,..."
    )]
    exclude: Vec<String>,

    /// Output dependency graph in Graphviz DOT format.
    #[arg(long, requires = "transitive")]
    dot: bool,

    /// Machine-readable JSON output.
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,

    /// Parallel fedrq queries (0 = CPUs).
    #[arg(short = 'j', long, default_value = "0", hide_default_value = true)]
    jobs: usize,
}

#[derive(Subcommand)]
enum Command {
    /// Compute parallel build phases for porting packages.
    BuildOrder(ResolveArgs),
    /// Analyze a crates.io crate's dependencies.
    CheckCrate(CheckCrateArgs),
    /// Check if an update would break reverse dependencies.
    CheckUpdate(CheckUpdateArgs),
    /// Detect dependency cycles in the build graph.
    FindCycles(ResolveArgs),
    /// Resolve the full dependency closure for porting.
    Resolve(ResolveArgs),
}

enum Mode {
    Resolve,
    BuildOrder,
    FindCycles,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // All subcommands need fedrq.
    if let Err(e) = sandogasa_cli::require_tool("fedrq", "sudo dnf install fedrq") {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    // CheckCrate and CheckUpdate have their own args; handle separately.
    if let Command::CheckCrate(a) = &cli.command {
        if a.jobs > 0 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(a.jobs)
                .build_global()
                .expect("failed to configure thread pool");
        }
        let opts = check_crate::CheckCrateOptions {
            branch: a.branch.clone(),
            repo: a.repo.clone(),
            verbose: a.verbose,
            transitive: a.transitive,
            include_dev: a.include_dev,
            include_optional: a.include_optional,
            include_too_old: a.include_too_old,
            exclude: a.exclude.iter().cloned().collect(),
        };
        return match check_crate::check_crate(&a.name, a.version.as_deref(), &opts) {
            Ok(report) => {
                if a.dot {
                    check_crate::print_dot(&report);
                } else if a.json {
                    print_json(&report);
                } else {
                    check_crate::print_report(&report);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    if let Command::CheckUpdate(a) = &cli.command {
        // check-update also needs koji for side tag queries.
        if let Err(e) = sandogasa_cli::require_tool("koji", "sudo dnf install koji") {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
        if a.jobs > 0 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(a.jobs)
                .build_global()
                .expect("failed to configure thread pool");
        }
        let opts = check_update::CheckUpdateOptions {
            branch: a.branch.clone(),
            repo: a.repo.clone(),
            testing_branch: a.testing_branch.clone(),
            koji_profile: a.koji_profile.clone(),
            verbose: a.verbose,
        };
        return match check_update::check_update(&a.input, &opts) {
            Ok(report) => {
                if a.json {
                    print_json(&report);
                } else {
                    check_update::print_report(&report);
                }
                let has_broken = report.reverse_deps.values().any(|r| r.status == "broken");
                if has_broken {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let (args, mode) = match &cli.command {
        Command::BuildOrder(a) => (a, Mode::BuildOrder),
        Command::CheckCrate(_) => unreachable!(),
        Command::CheckUpdate(_) => unreachable!(),
        Command::FindCycles(a) => (a, Mode::FindCycles),
        Command::Resolve(a) => (a, Mode::Resolve),
    };

    if args.source.is_none() && args.source_repo.is_none() {
        eprintln!("error: at least one of --source or --source-repo is required");
        return ExitCode::FAILURE;
    }
    if args.target.is_none() && args.target_repo.is_none() {
        eprintln!("error: at least one of --target or --target-repo is required");
        return ExitCode::FAILURE;
    }

    if args.refresh {
        if let Err(e) = sandogasa_fedrq::clear_cache() {
            eprintln!("error: failed to clear fedrq cache: {e}");
            return ExitCode::FAILURE;
        }
        if args.verbose {
            eprintln!("cleared fedrq cache");
        }
    }

    if args.jobs > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.jobs)
            .build_global()
            .expect("failed to configure thread pool");
    }

    let resolver = FedrqResolver {
        source: sandogasa_fedrq::Fedrq {
            branch: args.source.clone(),
            repo: args.source_repo.clone(),
        },
        target: sandogasa_fedrq::Fedrq {
            branch: args.target.clone(),
            repo: args.target_repo.clone(),
        },
    };
    let source_label = match (&args.source, &args.source_repo) {
        (Some(b), Some(r)) => format!("{b} ({r})"),
        (Some(b), None) => b.clone(),
        (None, Some(r)) => r.clone(),
        (None, None) => unreachable!(),
    };
    let target_label = match (&args.target, &args.target_repo) {
        (Some(b), Some(r)) => format!("{b} ({r})"),
        (Some(b), None) => b.clone(),
        (None, Some(r)) => r.clone(),
        (None, None) => unreachable!(),
    };
    let options = ResolveOptions {
        max_depth: args.max_depth,
        verbose: args.verbose,
        exclude_install: args.exclude_install.iter().cloned().collect(),
        auto_exclude: !args.no_auto_exclude,
    };
    let (closure, install_report) = if args.check_install {
        match resolve_with_installability(
            &resolver,
            &args.packages,
            &source_label,
            &target_label,
            &options,
        ) {
            Ok((c, r)) => (c, Some(r)),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match resolve_closure_with_options(
            &resolver,
            &args.packages,
            &source_label,
            &target_label,
            &options,
        ) {
            Ok(c) => (c, None),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    for w in &closure.warnings {
        eprintln!("warning: {w}");
    }

    if let Some(report) = &install_report {
        for (pkg, entry) in &report.issues {
            for u in &entry.unsatisfied {
                match &u.provided_by {
                    Some(provider) => {
                        eprintln!("install: {pkg}: {dep} (needs {provider})", dep = u.dep)
                    }
                    None => eprintln!("install: {pkg}: {dep} (unresolvable)", dep = u.dep),
                }
            }
        }
    }

    match mode {
        Mode::Resolve => {
            if args.json {
                if let Some(report) = &install_report {
                    print_json(&serde_json::json!({
                        "source_branch": closure.source_branch,
                        "target_branch": closure.target_branch,
                        "requested": closure.requested,
                        "closure": closure.closure,
                        "warnings": closure.warnings,
                        "installability": {
                            "issues": report.issues,
                            "additional_packages": report.additional_packages,
                        },
                    }));
                } else {
                    print_json(&closure);
                }
            } else {
                print_resolve(&closure);
                if let Some(report) = &install_report {
                    print_installability(report);
                }
            }
            ExitCode::SUCCESS
        }
        Mode::BuildOrder => {
            let edges = closure.to_edges();
            match dag::topological_layers(&edges) {
                Ok(phases) => {
                    if args.copr {
                        print_copr_script(&phases);
                    } else if args.koji {
                        print_koji_chain(&phases);
                    } else if args.json {
                        let mut json = serde_json::json!({
                            "source_branch": closure.source_branch,
                            "target_branch": closure.target_branch,
                            "requested": closure.requested,
                            "build_order": phases,
                        });
                        if let Some(report) = &install_report {
                            json["installability"] = serde_json::json!({
                                "issues": report.issues,
                                "additional_packages":
                                    report.additional_packages,
                            });
                        }
                        print_json(&json);
                    } else {
                        print_build_order(&phases, &closure);
                        if let Some(report) = &install_report {
                            print_installability(report);
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(_) => {
                    eprintln!(
                        "error: dependency graph contains cycles; \
                        run 'find-cycles' for details"
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Mode::FindCycles => {
            let edges = closure.to_edges();
            let cycles = dag::find_cycles(&edges);
            if args.json {
                print_json(&serde_json::json!({
                    "source_branch": closure.source_branch,
                    "target_branch": closure.target_branch,
                    "requested": closure.requested,
                    "cycles": cycles,
                }));
            } else {
                print_cycles(&cycles, &closure);
            }
            if cycles.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn print_copr_script(phases: &[dag::BuildPhase]) {
    println!(
        r#"#!/bin/bash
# Generated by ebranch build-order --copr
# Usage: ./script.sh <copr-repo> [extra copr build-package flags...]
set -euo pipefail

REPO="${{1:?Usage: $0 <copr-repo> [extra flags...]}}"
shift
EXTRA=("$@")

extract_build_id() {{
    # Parse "Created builds: <id>" from copr output
    grep -oP 'Created builds: \K[0-9]+' | head -1
}}"#
    );

    for (i, phase) in phases.iter().enumerate() {
        println!();
        println!("# Phase {}", phase.phase);

        for (j, pkg) in phase.packages.iter().enumerate() {
            if i == 0 && j == 0 {
                // Very first package: no dependency flags, capture batch ID.
                println!(
                    r#"PHASE_{phase}_ID=$(copr build-package --nowait --name {pkg} "$REPO" "${{EXTRA[@]+"${{EXTRA[@]}}"}}" 2>&1 | tee /dev/stderr | extract_build_id)"#,
                    phase = phase.phase,
                    pkg = pkg,
                );
            } else if j == 0 {
                // First package in a new phase: depends on previous phase.
                println!(
                    r#"PHASE_{phase}_ID=$(copr build-package --nowait --after-build-id "$PHASE_{prev}_ID" --name {pkg} "$REPO" "${{EXTRA[@]+"${{EXTRA[@]}}"}}" 2>&1 | tee /dev/stderr | extract_build_id)"#,
                    phase = phase.phase,
                    prev = phases[i - 1].phase,
                    pkg = pkg,
                );
            } else {
                // Subsequent package in same phase: same batch.
                println!(
                    r#"copr build-package --nowait --with-build-id "$PHASE_{phase}_ID" --name {pkg} "$REPO" "${{EXTRA[@]+"${{EXTRA[@]}}"}}""#,
                    phase = phase.phase,
                    pkg = pkg,
                );
            }
        }
    }
}

fn print_koji_chain(phases: &[dag::BuildPhase]) {
    let chain: Vec<String> = phases
        .iter()
        .map(|phase| phase.packages.join(" "))
        .collect();
    println!("{}", chain.join(" : "));
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("JSON serialization failed")
    );
}

fn print_resolve(closure: &resolve::Closure) {
    println!(
        "Dependency closure from {} to {}:\n",
        closure.source_branch, closure.target_branch
    );

    let discovered = closure.closure.len() - closure.requested.len();
    for (pkg, entry) in &closure.closure {
        if entry.missing_deps.is_empty() {
            println!("  {pkg}: all BuildRequires satisfied");
        } else {
            println!("  {pkg}:");
            for dep in &entry.missing_deps {
                println!("    - {} (provided by {})", dep.dep, dep.provided_by);
            }
        }
    }

    println!(
        "\nTotal: {} package(s) in closure ({} requested, {} discovered).",
        closure.closure.len(),
        closure.requested.len(),
        discovered
    );
}

fn print_build_order(phases: &[dag::BuildPhase], closure: &resolve::Closure) {
    println!(
        "Build order from {} to {}:\n",
        closure.source_branch, closure.target_branch
    );

    for phase in phases {
        println!("  Phase {}:", phase.phase);
        for pkg in &phase.packages {
            println!("    - {pkg}");
        }
    }

    println!(
        "\n{} package(s) in {} phase(s).",
        closure.closure.len(),
        phases.len()
    );
}

fn print_installability(report: &resolve::InstallabilityReport) {
    if report.issues.is_empty() {
        println!("\nInstallability: all subpackage Requires satisfied.");
        return;
    }

    println!("\nInstallability issues:\n");
    for (pkg, entry) in &report.issues {
        println!("  {pkg}:");
        for u in &entry.unsatisfied {
            match &u.provided_by {
                Some(provider) => {
                    println!("    - {} (needs {})", u.dep, provider);
                }
                None => {
                    println!("    - {} (unresolvable)", u.dep);
                }
            }
        }
    }

    if !report.additional_packages.is_empty() {
        println!("\nAdditional packages needed for installability:");
        for pkg in &report.additional_packages {
            println!("  - {pkg}");
        }
    }
}

fn print_cycles(cycles: &[dag::Cycle], closure: &resolve::Closure) {
    println!(
        "Cycle detection from {} to {}:\n",
        closure.source_branch, closure.target_branch
    );

    if cycles.is_empty() {
        println!("  No cycles detected. The dependency graph is a DAG.");
    } else {
        println!("  Found {} cycle(s):\n", cycles.len());
        for (i, cycle) in cycles.iter().enumerate() {
            let chain: Vec<&str> = cycle
                .packages
                .iter()
                .map(|s| s.as_str())
                .chain(std::iter::once(cycle.packages[0].as_str()))
                .collect();
            println!("  Cycle {} ({} packages):", i + 1, cycle.packages.len());
            println!("    {}", chain.join(" -> "));
        }
    }
}
