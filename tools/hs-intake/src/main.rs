// SPDX-License-Identifier: Apache-2.0 OR MIT

use clap::{Args, Parser, Subcommand};
use hs_intake::{
    compare, compare_buildrequires, compare_provides, compare_requires, fedrq, safe_to_backport,
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
    command: Commands,
}

/// Positional arguments and flags shared by the three
/// `compare-*` subcommands.
#[derive(Args)]
struct CompareArgs {
    /// Source RPM name (e.g. "systemd").
    srpm: String,
    /// Branch to compare from (e.g. "rawhide").
    source_branch: String,
    /// Branch to compare to (e.g. "c10s-hyperscale").
    target_branch: String,
    /// Output as JSON.
    #[arg(long)]
    json: bool,
    /// Also show unchanged entries.
    #[arg(long)]
    show_unchanged: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Compare the BuildRequires of a source package between two branches.
    CompareBuildRequires(CompareArgs),
    /// Compare the Provides of a source package between two branches.
    CompareProvides(CompareArgs),
    /// Compare the Requires of a source package between two branches.
    CompareRequires(CompareArgs),
    /// Check if a source package is safe to backport between branches.
    SafeToBackport {
        /// Source RPM name (e.g. "systemd").
        srpm: String,
        /// Branch to backport to (e.g. "c10s-hyperscale").
        target_branch: String,
        /// Branch to take the package from (e.g. "rawhide").
        source_branch: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        /// Additional branches to check for reverse dependencies (comma-separated).
        #[arg(long, value_delimiter = ',')]
        also_check: Vec<String>,
    },
}

/// Run one `compare-*` subcommand: `compare` produces the diff for
/// the attribute, `label` names it in the rendered output.
fn run_compare(
    args: &CompareArgs,
    label: &str,
    compare_fn: fn(&str, &str, &str) -> Result<compare::CompareResult, fedrq::Error>,
) {
    match compare_fn(&args.srpm, &args.source_branch, &args.target_branch) {
        Ok(cmp) => {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&cmp).unwrap());
            } else {
                compare::print_result(
                    &cmp,
                    label,
                    &args.source_branch,
                    &args.target_branch,
                    args.show_unchanged,
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));

    match cli.command {
        Commands::CompareBuildRequires(args) => run_compare(
            &args,
            "BuildRequire",
            compare_buildrequires::compare_buildrequires,
        ),
        Commands::CompareProvides(args) => {
            run_compare(&args, "Provide", compare_provides::compare_provides)
        }
        Commands::CompareRequires(args) => {
            run_compare(&args, "Require", compare_requires::compare_requires)
        }
        Commands::SafeToBackport {
            srpm,
            target_branch,
            source_branch,
            json,
            also_check,
        } => {
            match safe_to_backport::safe_to_backport(
                &srpm,
                &target_branch,
                &source_branch,
                &also_check,
            ) {
                Ok(result) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&result).unwrap());
                    } else {
                        safe_to_backport::print_result(
                            &result,
                            &srpm,
                            &target_branch,
                            &source_branch,
                        );
                    }
                    if !result.safe {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}
