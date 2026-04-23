// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod list;

use list::ListArgs;

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::List(args) => list::run(&args),
    }
}
