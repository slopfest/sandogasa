// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod config;
mod configure;
mod dump_inventory;
mod file_issue;
mod gitlab;
mod jira;
mod list_issues;
mod ping;
mod retire;
mod status;
#[cfg(test)]
mod test_support;
mod timeline;
mod untag;
mod utils;

use dump_inventory::DumpInventoryArgs;
use file_issue::FileIssueArgs;
use list_issues::ListIssuesArgs;
use ping::PingArgs;
use retire::RetireArgs;
use status::StatusArgs;
use timeline::TimelineArgs;
use untag::UntagArgs;

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
    /// Set up GitLab and JIRA authentication tokens.
    Config,

    /// Enumerate packages in a proposed_updates Koji tag and
    /// emit a sandogasa-inventory TOML file.
    DumpInventory(DumpInventoryArgs),

    /// File a tracking issue in the proposed_updates GitLab
    /// group for a given Merge Request URL.
    FileIssue(FileIssueArgs),

    /// List the SIG's tracking issues per release and package, with
    /// the MR each names; with an inventory, the packages missing one
    ListIssues(ListIssuesArgs),

    /// Nudge upstream merge requests that have gone quiet, and
    /// show the last response on those that have not.
    Ping(PingArgs),

    /// Close a tracking issue for a reason — landed, superseded or
    /// abandoned — labelling it, telling the MR, closing the MR of
    /// an abandoned change.
    Retire(RetireArgs),

    /// Report JIRA status and suggested next action for each
    /// active tracking issue.
    Status(StatusArgs),

    /// How long each Proposed Update took and where the time went:
    /// filed, covered, fixed in stock, RHEL's advisory, shadowed.
    Timeline(TimelineArgs),

    /// Untag a proposed_updates build from its CBS -release tag
    /// after verifying the JIRA is resolved.
    Untag(UntagArgs),
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
    match cli.command {
        Command::Config => configure::run(),
        Command::DumpInventory(args) => dump_inventory::run(&args),
        Command::FileIssue(args) => file_issue::run(&args),
        Command::ListIssues(args) => list_issues::run(&args),
        Command::Ping(args) => ping::run(&args),
        Command::Retire(args) => retire::run(&args),
        Command::Status(args) => status::run(&args),
        Command::Timeline(args) => timeline::run(&args),
        Command::Untag(args) => untag::run(&args),
    }
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/cpu-sig-tracker.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }
}
