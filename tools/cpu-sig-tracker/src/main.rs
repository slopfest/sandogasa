// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod dump_inventory;

use dump_inventory::DumpInventoryArgs;

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
    /// Enumerate packages in a proposed_updates Koji tag and
    /// emit a sandogasa-inventory TOML file.
    DumpInventory(DumpInventoryArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::DumpInventory(args) => dump_inventory::run(&args),
    }
}
