// SPDX-License-Identifier: MPL-2.0

use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
    ///
    /// The script accepts the Copr repo as its first argument,
    /// followed by any extra flags to pass to copr build-package.
    #[arg(long)]
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

    /// Max recursion depth (0 = unlimited).
    #[arg(long, default_value = "0")]
    max_depth: usize,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,

    /// Clear fedrq repo metadata cache before querying.
    #[arg(long)]
    refresh: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Compute parallel build phases for porting packages.
    BuildOrder(ResolveArgs),
    /// Detect dependency cycles in the build graph.
    FindCycles(ResolveArgs),
    /// Resolve the full dependency closure for porting.
    Resolve(ResolveArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (args, mode) = match &cli.command {
        Command::BuildOrder(a) => (a, Mode::BuildOrder),
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

enum Mode {
    Resolve,
    BuildOrder,
    FindCycles,
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
