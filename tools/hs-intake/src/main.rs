mod compare;
mod compare_buildrequires;
mod compare_provides;
mod compare_requires;
mod safe_to_backport;

mod fedrq;
mod rpmvercmp;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Hyperscale package intake tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compare the BuildRequires of a source package between two branches.
    CompareBuildRequires {
        /// Source RPM name (e.g. "systemd").
        srpm: String,
        /// Branch to compare from (e.g. "rawhide").
        source_branch: String,
        /// Branch to compare to (e.g. "c10s-hyperscale").
        target_branch: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Compare the Provides of a source package between two branches.
    CompareProvides {
        /// Source RPM name (e.g. "systemd").
        srpm: String,
        /// Branch to compare from (e.g. "rawhide").
        source_branch: String,
        /// Branch to compare to (e.g. "c10s-hyperscale").
        target_branch: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Compare the Requires of a source package between two branches.
    CompareRequires {
        /// Source RPM name (e.g. "systemd").
        srpm: String,
        /// Branch to compare from (e.g. "rawhide").
        source_branch: String,
        /// Branch to compare to (e.g. "c10s-hyperscale").
        target_branch: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
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
    },
}

fn run_compare(
    result: Result<compare::CompareResult, fedrq::Error>,
    label: &str,
    source_branch: &str,
    target_branch: &str,
    json: bool,
) {
    match result {
        Ok(cmp) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&cmp).unwrap());
            } else {
                compare::print_result(&cmp, label, source_branch, target_branch);
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::CompareBuildRequires {
            srpm,
            source_branch,
            target_branch,
            json,
        } => {
            let result = compare_buildrequires::compare_buildrequires(
                &srpm,
                &source_branch,
                &target_branch,
            );
            run_compare(result, "BuildRequire", &source_branch, &target_branch, json);
        }
        Commands::CompareProvides {
            srpm,
            source_branch,
            target_branch,
            json,
        } => {
            let result =
                compare_provides::compare_provides(&srpm, &source_branch, &target_branch);
            run_compare(result, "Provide", &source_branch, &target_branch, json);
        }
        Commands::CompareRequires {
            srpm,
            source_branch,
            target_branch,
            json,
        } => {
            let result =
                compare_requires::compare_requires(&srpm, &source_branch, &target_branch);
            run_compare(result, "Require", &source_branch, &target_branch, json);
        }
        Commands::SafeToBackport {
            srpm,
            target_branch,
            source_branch,
            json,
        } => {
            match safe_to_backport::safe_to_backport(&srpm, &target_branch, &source_branch) {
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
