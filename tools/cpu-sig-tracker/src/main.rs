// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod config;
mod configure;
mod dump_inventory;
mod file_issue;
mod gitlab;
mod jira;
mod retire;
mod status;
mod sync_issues;
#[cfg(test)]
mod test_support;
mod untag;
mod utils;

use dump_inventory::DumpInventoryArgs;
use file_issue::FileIssueArgs;
use retire::RetireArgs;
use status::StatusArgs;
use sync_issues::SyncIssuesArgs;
use untag::UntagArgs;

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

    /// Close a tracking issue (retire-issue suggestion) after
    /// verifying JIRA is resolved and the build is untagged.
    Retire(RetireArgs),

    /// Report JIRA status and suggested next action for each
    /// active tracking issue.
    Status(StatusArgs),

    /// Report which inventory packages have active, proposed,
    /// or missing tracking issues per release.
    SyncIssues(SyncIssuesArgs),

    /// Untag a proposed_updates build from its CBS -release tag
    /// after verifying the JIRA is resolved.
    Untag(UntagArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Config => configure::run(),
        Command::DumpInventory(args) => dump_inventory::run(&args),
        Command::FileIssue(args) => file_issue::run(&args),
        Command::Retire(args) => retire::run(&args),
        Command::Status(args) => status::run(&args),
        Command::SyncIssues(args) => sync_issues::run(&args),
        Command::Untag(args) => untag::run(&args),
    }
}
