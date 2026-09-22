// SPDX-License-Identifier: Apache-2.0 OR MIT

//! fesco-chair — helper for FESCo meeting chair duties: the agenda
//! announcement, the day-of meetbot script, the post-meeting
//! summary email, and the state of open in-ticket votes. See
//! <https://fedoraproject.org/wiki/FESCo_meeting_process>.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod agenda;
mod config;
mod script;
mod sources;
mod state;
mod summary;
mod votes;

#[derive(Parser)]
#[command(
    about,
    long_about = None,
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
    /// Compose the meeting announcement email from the tracker.
    Agenda(agenda::AgendaArgs),
    /// Store the Forgejo API token (interactive).
    Config,
    /// Day-of checklist plus the meetbot command script.
    Script(script::ScriptArgs),
    /// Compose the post-meeting summary email from the minutes.
    Summary(summary::SummaryArgs),
    /// Where each open in-ticket vote stands under the policy.
    Votes(votes::VotesArgs),
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    match sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME")).command {
        Command::Agenda(args) => agenda::run(&args),
        Command::Config => match config::cmd_config() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Command::Script(args) => script::run(&args),
        Command::Summary(args) => summary::run(&args),
        Command::Votes(args) => votes::run(&args),
    }
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/fesco-chair.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }
}
