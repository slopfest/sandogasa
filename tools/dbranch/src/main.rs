// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use dbranch::plan;
use dbranch::rebuild::{self, ChrootRefresh, Options};
use dbranch::ui::Ui;

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
    /// Fix up gbp.conf / salsa-ci.yml on existing PPA branch(es).
    Fixup {
        /// Branch(es) to fix up (default: the current branch).
        #[arg(value_name = "BRANCH")]
        branches: Vec<String>,

        /// Run in this package working directory.
        #[arg(short = 'C', long, default_value = ".", value_name = "DIR")]
        repo: PathBuf,

        /// Print the commands without running anything (a tutorial).
        #[arg(long)]
        dry_run: bool,

        /// Run, but narrate each step + command first (follow along).
        #[arg(long)]
        explain: bool,

        /// Suppress tool output, showing it only when a step fails.
        #[arg(short, long, conflicts_with = "explain")]
        quiet: bool,
    },

    /// Rebuild a Debian package across its Ubuntu/PPA branches.
    Rebuild {
        /// PPA branch(es) to rebuild (repeatable or CSV).
        #[arg(
            value_delimiter = ',',
            value_name = "BRANCH",
            long_help = "\
PPA branch(es) to rebuild, repeatable or comma-
separated. Run from the Debian branch (the merge
source). A branch that doesn't exist is created from
it (codename = the name's basename). With none given,
all local branches except the current one and gbp's
upstream / pristine-tar branches are rebuilt."
        )]
        branches: Vec<String>,

        /// Run in this package working directory.
        #[arg(short = 'C', long, default_value = ".", value_name = "DIR")]
        repo: PathBuf,

        /// Stages to run (repeatable or CSV).
        #[arg(
            long,
            value_delimiter = ',',
            value_name = "STAGE",
            help_heading = "Stages",
            long_help = "\
Stages to run, repeatable or comma-separated:
  merge   merge the Debian branch + write the rebuild
          changelog entry
  build   debuild + pbuilder-dist
  lint    lintian on the built source package (warns,
          does not fail the run)
  push    git push the branch, then watch its CI
          pipeline via glab (see --nowait)
  upload  dput the built package (needs --ppa or
          --upload-target)
  tag     dh clean + gbp tag the release
  all     merge + build + lint + push
          (upload and tag are opt-in)
Defaults to `merge` (the others are opt-in for now)."
        )]
        stage: Vec<String>,

        /// Merge source branch (default: the checked-out branch).
        #[arg(long, value_name = "BRANCH", help_heading = "Stages")]
        source: Option<String>,

        /// In the push stage, push but don't wait for / watch CI.
        #[arg(long, help_heading = "Stages")]
        nowait: bool,

        /// Build stage: force-refresh the pbuilder chroot first.
        #[arg(long, help_heading = "Stages", conflicts_with = "no_refresh_chroot")]
        refresh_chroot: bool,

        /// Build stage: never auto-refresh the pbuilder chroot.
        #[arg(long, help_heading = "Stages")]
        no_refresh_chroot: bool,

        /// Bulk run: skip the branch-set confirmation prompt.
        #[arg(short = 'y', long, help_heading = "Bulk (no branches given)")]
        yes: bool,

        /// Bulk run: include EOL Ubuntu releases (default skips them).
        #[arg(long, help_heading = "Bulk (no branches given)")]
        include_eol: bool,

        /// Upload stage: target PPA (e.g. `user/name`; `ppa:` optional).
        #[arg(
            long,
            value_name = "PPA",
            help_heading = "Upload",
            conflicts_with = "upload_target"
        )]
        ppa: Option<String>,

        /// Upload stage: dput target host (e.g. `mentors`, `ftp-master`).
        #[arg(long, value_name = "TARGET", help_heading = "Upload")]
        upload_target: Option<String>,

        /// Print the commands without running anything (a tutorial).
        #[arg(long, help_heading = "Output")]
        dry_run: bool,

        /// Run, but narrate each step + command first (follow along).
        #[arg(long, help_heading = "Output")]
        explain: bool,

        /// Suppress tool output, showing it only when a step fails.
        #[arg(short, long, help_heading = "Output", conflicts_with = "explain")]
        quiet: bool,
    },

    /// Watch a branch's GitLab CI pipeline via glab.
    WatchCi {
        /// Branch to watch (defaults to the current branch).
        #[arg(value_name = "BRANCH")]
        branch: Option<String>,

        /// Run in this package working directory.
        #[arg(short = 'C', long, default_value = ".", value_name = "DIR")]
        repo: PathBuf,

        /// Print the commands without running anything (a tutorial).
        #[arg(long)]
        dry_run: bool,

        /// Run, but narrate each step + command first (follow along).
        #[arg(long)]
        explain: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dbranch: {e}");
            // Propagate a failing stage command's real exit code; any
            // other error is a generic failure.
            let code = e
                .downcast_ref::<dbranch::ui::StageFailure>()
                .map(|f| f.code)
                .unwrap_or(1);
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
    }
}

fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Fixup {
            branches,
            repo,
            dry_run,
            explain,
            quiet,
        } => {
            let ui = Ui {
                explain,
                dry_run,
                quiet,
            };
            rebuild::fixup(&ui, &repo, branches)
        }
        Command::Rebuild {
            branches,
            repo,
            stage,
            source,
            nowait,
            refresh_chroot,
            no_refresh_chroot,
            yes,
            include_eol,
            ppa,
            upload_target,
            dry_run,
            explain,
            quiet,
        } => {
            let ui = Ui {
                explain,
                dry_run,
                quiet,
            };
            let stages = rebuild::parse_stages(&stage)?;
            // --ppa is sugar for a `ppa:<name>` dput target.
            let upload_target = ppa.map(|p| plan::ppa_target(&p)).or(upload_target);
            let chroot_refresh = if refresh_chroot {
                ChrootRefresh::Force
            } else if no_refresh_chroot {
                ChrootRefresh::Never
            } else {
                ChrootRefresh::Auto
            };
            let opts = Options {
                branches,
                stages,
                nowait,
                upload_target,
                source,
                chroot_refresh,
                assume_yes: yes,
                include_eol,
            };
            rebuild::run(&ui, &repo, &opts)
        }
        Command::WatchCi {
            branch,
            repo,
            dry_run,
            explain,
        } => {
            let ui = Ui {
                explain,
                dry_run,
                quiet: false,
            };
            rebuild::watch_ci(&ui, &repo, branch)
        }
    }
}
