// SPDX-License-Identifier: Apache-2.0 OR MIT

//! fesco-chair — helper for FESCo meeting chair duties: the agenda
//! announcement, the day-of meetbot script, and the post-meeting
//! summary email. See
//! <https://fedoraproject.org/wiki/FESCo_meeting_process>.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod agenda;
mod config;
mod script;
mod sources;
mod state;
mod summary;

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
    /// Compose the meeting announcement email from the tracker.
    Agenda(agenda::AgendaArgs),
    /// Store the Forgejo API token (interactive).
    Config,
    /// Day-of checklist plus the meetbot command script.
    Script(script::ScriptArgs),
    /// Compose the post-meeting summary email from the minutes.
    Summary(summary::SummaryArgs),
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
    }
}
