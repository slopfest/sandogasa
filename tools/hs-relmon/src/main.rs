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
        #[arg(short, long, long_help = "\
Comma-separated list of distros to check.

Valid names:
  upstream         Newest version across all repos
  fedora           Fedora Rawhide + latest stable
  fedora-rawhide   Fedora Rawhide only
  fedora-stable    Latest stable Fedora only
  centos           Latest CentOS Stream
  centos-stream    Latest CentOS Stream
  hyperscale / hs  Hyperscale EL9 + EL10
  hs9              Hyperscale EL9 only
  hs10             Hyperscale EL10 only")]
        distros: Option<String>,

        /// Distribution to compare Hyperscale builds against.
        #[arg(long, default_value = "upstream", long_help = "\
Distribution to compare Hyperscale builds against.

Valid names:
  upstream         Newest version across all repos (default)
  fedora-rawhide   Fedora Rawhide
  fedora-stable    Latest stable Fedora
  centos           Latest CentOS Stream
  centos-stream    Latest CentOS Stream")]
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
