// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod list;
mod sync;

use list::ListArgs;
use sync::SyncArgs;

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
    /// List SIG meetings recorded on meetbot.
    List(ListArgs),
    /// Sync SIG meetings into a tool-managed markdown list file.
    Sync(SyncArgs),
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    match cli.command {
        Command::List(args) => list::run(&args),
        Command::Sync(args) => sync::run(&args),
    }
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/hs-meetings.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }
}
