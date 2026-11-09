// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod add_pkg;
mod build;
mod config;
mod configure;
mod sig_repo;

use add_pkg::AddPkgArgs;
use build::BuildArgs;
use sig_repo::SigRepoArgs;

/// Packaging for CentOS SIGs that build in the CentOS Build System
/// (CBS), knowing each SIG's conventions: where its package repos live,
/// which branch builds into which collection, and what its targets and
/// tags are called. Proposed Updates and Hyperscale are known.
#[derive(Parser)]
#[command(
    about,
    max_term_width = 80,
    version = sandogasa_cli::version!(),
    before_help = sandogasa_cli::banner!()
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List a package in the CBS tags its branch builds through, as
    /// its first build needs
    AddPkg(AddPkgArgs),

    /// Build the checked-out SIG branch in CBS: the target, the SIG
    /// repo's URL and the commit worked out from the branch
    Build(BuildArgs),

    /// Set up the GitLab token.
    Config(sandogasa_cli::ConfigArgs),

    /// Make sure the SIG has its repo for a package, a pull mirror of
    /// CentOS Stream's, and push a merge request's change to it
    SigRepo(SigRepoArgs),
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    match cli.command {
        Command::AddPkg(args) => add_pkg::run(&args),
        Command::Build(args) => build::run(&args),
        Command::Config(args) => configure::run(&args),
        Command::SigRepo(args) => sig_repo::run(&args),
    }
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/cbspkg.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }
}
