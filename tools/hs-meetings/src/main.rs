// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod list;
mod sync;

use list::ListArgs;
use sync::SyncArgs;

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
    /// List SIG meetings recorded on meetbot.
    List(ListArgs),
    /// Sync SIG meetings into a tool-managed markdown list file.
    Sync(SyncArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::List(args) => list::run(&args),
        Command::Sync(args) => sync::run(&args),
    }
}
