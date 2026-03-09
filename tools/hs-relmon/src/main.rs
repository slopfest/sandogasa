// SPDX-License-Identifier: MPL-2.0

use clap::{Parser, Subcommand};
use hs_relmon::cbs;
use hs_relmon::check_latest::{self, Distros, TrackRef};
use hs_relmon::repology;

#[derive(Parser)]
#[command(name = "hs-relmon", about = "Hyperscale release monitoring")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the latest version of a package across distributions.
    CheckLatest {
        /// Source package name (e.g. ethtool).
        package: String,

        /// Comma-separated list of distros to check.
        ///
        /// Valid names: upstream, fedora (rawhide + stable), fedora-rawhide,
        /// fedora-stable, centos, centos-stream, hyperscale (9 + 10), hs,
        /// hs9, hs10.
        #[arg(short, long)]
        distros: Option<String>,

        /// Distribution to compare Hyperscale builds against.
        ///
        /// Valid names: upstream (default), fedora-rawhide, fedora-stable,
        /// centos, centos-stream.
        #[arg(long, default_value = "upstream")]
        track: String,

        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::CheckLatest {
            package,
            distros,
            track,
            json,
        } => {
            let distros = match distros {
                Some(s) => Distros::parse(&s)?,
                None => Distros::all(),
            };
            let track = TrackRef::parse(&track)?;

            let repology_client = repology::Client::new();
            let cbs_client = cbs::Client::new();
            let result =
                check_latest::check(&repology_client, &cbs_client, &package, &distros, &track)?;

            if json {
                check_latest::print_json(&result)?;
            } else {
                check_latest::print_table(&result);
            }
        }
    }

    Ok(())
}
