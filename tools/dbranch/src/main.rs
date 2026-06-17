// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use dbranch::rebuild::{self, Options};
use dbranch::ui::Ui;

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
    /// Rebuild a Debian package across its Ubuntu/PPA branches.
    Rebuild {
        /// PPA branch(es) to rebuild (repeatable or CSV).
        #[arg(
            value_delimiter = ',',
            value_name = "BRANCH",
            long_help = "\
PPA branch(es) to rebuild, repeatable or comma-
separated. Run from the Debian branch (the merge
source). A branch that doesn't exist is created from
it (codename = the name's basename). With none given,
all local branches except the current one and gbp's
upstream / pristine-tar branches are rebuilt."
        )]
        branches: Vec<String>,

        /// Run in this package working directory.
        #[arg(short = 'C', long, default_value = ".", value_name = "DIR")]
        repo: PathBuf,

        /// Stages to run (repeatable or CSV).
        #[arg(
            long,
            value_delimiter = ',',
            value_name = "STAGE",
            long_help = "\
Stages to run, repeatable or comma-separated:
  merge   merge the Debian branch + write the rebuild
          changelog entry
  build   debuild + pbuilder-dist
  all     both
Defaults to `merge` (build is opt-in for now)."
        )]
        stage: Vec<String>,

        /// Print the commands without running anything (a tutorial).
        #[arg(long)]
        dry_run: bool,

        /// Run, but narrate each step + command first (follow along).
        #[arg(long)]
        explain: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dbranch: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Rebuild {
            branches,
            repo,
            stage,
            dry_run,
            explain,
        } => {
            let ui = Ui { explain, dry_run };
            let stages = rebuild::parse_stages(&stage)?;
            let opts = Options { branches, stages };
            rebuild::run(&ui, &repo, &opts)
        }
    }
}
