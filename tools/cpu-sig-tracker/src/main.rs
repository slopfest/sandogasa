// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod config;
mod configure;
mod dump_inventory;
mod file_issue;
mod gitlab;
mod jira;

use dump_inventory::DumpInventoryArgs;
use file_issue::FileIssueArgs;

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
    /// Set up GitLab and JIRA authentication tokens.
    Config,

    /// Enumerate packages in a proposed_updates Koji tag and
    /// emit a sandogasa-inventory TOML file.
    DumpInventory(DumpInventoryArgs),

    /// File a tracking issue in the proposed_updates GitLab
    /// group for a given Merge Request URL.
    FileIssue(FileIssueArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Config => configure::run(),
        Command::DumpInventory(args) => dump_inventory::run(&args),
        Command::FileIssue(args) => file_issue::run(&args),
    }
}
