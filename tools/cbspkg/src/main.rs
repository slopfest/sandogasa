// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod add_pkg;
mod build;
mod config;
mod configure;
mod local;
mod promote;
mod sig_repo;
#[cfg(test)]
mod test_support;
mod upload_sources;

use add_pkg::AddPkgArgs;
use build::BuildArgs;
use local::{MockbuildArgs, SrpmArgs, VerrelArgs};
use promote::PromoteArgs;
use sig_repo::SigRepoArgs;
use upload_sources::UploadSourcesArgs;

/// Packaging for CentOS SIGs that build in the CentOS Build System
/// (CBS), knowing each SIG's conventions: where its package repos live,
/// which branch builds into which collection, and what its targets and
/// tags are called. Proposed Updates and Hyperscale are known.
///
/// add-pkg, build, promote and upload-sources act in CBS or the SIG
/// lookaside in your name; see "Commands run by a person" in
/// sandogasa's README.
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

    /// Build the checked-out SIG branch locally in mock, in the config
    /// matching its collection, with CBS's macros
    Mockbuild(MockbuildArgs),

    /// Move a build to its collection's next stage (-testing, then
    /// -release, untagging it from -testing)
    Promote(PromoteArgs),

    /// Make sure the SIG has its repo for a package, a pull mirror of
    /// CentOS Stream's, and push a merge request's change to it
    SigRepo(SigRepoArgs),

    /// Build the checked-out SIG branch's SRPM, with the dist tag CBS
    /// would give it
    Srpm(SrpmArgs),

    /// Upload the files a branch's sources names to the SIG lookaside,
    /// where CBS fetches them
    UploadSources(UploadSourcesArgs),

    /// Print the version-release the checked-out SIG branch builds in
    /// CBS, dist tag included
    Verrel(VerrelArgs),
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    match cli.command {
        Command::AddPkg(args) => add_pkg::run(&args),
        Command::Build(args) => build::run(&args),
        Command::Config(args) => configure::run(&args),
        Command::Mockbuild(args) => local::run_mockbuild(&args),
        Command::Promote(args) => promote::run(&args),
        Command::SigRepo(args) => sig_repo::run(&args),
        Command::Srpm(args) => local::run_srpm(&args),
        Command::UploadSources(args) => upload_sources::run(&args),
        Command::Verrel(args) => local::run_verrel(&args),
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
