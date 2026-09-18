// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use dbranch::plan;
use dbranch::rebuild::{self, ChrootRefresh, Options, UpdateOptions};
use dbranch::ui::Ui;
use dbranch::upstream::{self, CloneOptions};

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
    /// Clone upstream's git to package it straight from its tags.
    #[command(long_about = "\
Clone upstream's git to package it straight from its tags (gbp's
\"upstream uses git, no tarballs\" flow): clone with upstream as
remote `upstream`, start the Debian branch at the newest release tag,
and commit a debian/gbp.conf naming the tag style (upstream-tag) with
pristine-tar enabled. When upstream itself carries a debian/* branch,
offers to start from it instead (its packaging commits stay in the
history, so diverging is ordinary commits), merging the tag in.
--salsa NAMESPACE also creates the packaging project on salsa and adds
it as origin (the first push is `update --stage push`, which adds the
CI file); --mr registers the clone with myrepos.
Later releases are then merged in by `update`,
which detects this layout, and the source stage generates the orig
tarball from the tag with `gbp export-orig`. Writing the rest of
debian/ is left to you.")]
    Clone {
        /// Upstream git URL.
        #[arg(value_name = "URL")]
        url: String,

        /// Directory to clone into (default: the repository's name).
        #[arg(value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Upstream release to start from (default: the newest tag).
        #[arg(long, value_name = "VERSION")]
        upstream_version: Option<String>,

        /// Debian packaging branch to create.
        #[arg(long, value_name = "BRANCH", default_value = "debian/latest")]
        debian_branch: String,

        /// Name for the remote holding upstream's git.
        #[arg(long, value_name = "NAME", default_value = "upstream")]
        upstream_remote: String,

        /// Start from upstream's own debian/* branch without asking.
        #[arg(long, conflicts_with = "fresh")]
        from_upstream_packaging: bool,

        /// Ignore any debian/* branch upstream carries; start at the tag.
        #[arg(long)]
        fresh: bool,

        /// Create the packaging project on salsa.debian.org in this
        /// namespace and add it as origin (pushing is `update`'s job).
        #[arg(long, value_name = "NAMESPACE")]
        salsa: Option<String>,

        /// Register the clone with myrepos (mr config).
        #[arg(long)]
        mr: bool,

        /// mrconfig to register in (default: ~/.mrconfig).
        #[arg(long, value_name = "PATH", requires = "mr")]
        mrconfig: Option<PathBuf>,

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
  source  debuild -S the source package (-sa unless
          uploading to the Debian archive)
  build   pbuilder-dist scratch build of the .dsc
  lint    lintian on the built source package (warns,
          does not fail the run)
  push    git push the branch, then watch its CI
          pipeline via glab (see --nowait)
  upload  dput the built package (needs --ppa or
          --upload-target)
  tag     dh clean + gbp tag the release
  all     merge + source + build + lint + push
          (upload and tag are opt-in)
Defaults to `merge` (the others are opt-in for now)."
        )]
        stage: Vec<String>,

        /// Merge source branch (default: the checked-out branch).
        #[arg(long, value_name = "BRANCH", help_heading = "Stages")]
        source: Option<String>,

        /// Git remote to push to and configure CI on (default: the
        /// branch's own, else the only one; asks when several could).
        #[arg(long, value_name = "NAME", help_heading = "Stages")]
        remote: Option<String>,

        /// In the push stage, push but don't wait for / watch CI.
        #[arg(long, help_heading = "Stages")]
        nowait: bool,

        /// Build stage: force-refresh the pbuilder chroot first.
        #[arg(long, help_heading = "Stages", conflicts_with = "no_refresh_chroot")]
        refresh_chroot: bool,

        /// Build stage: never auto-refresh the pbuilder chroot.
        #[arg(long, help_heading = "Stages")]
        no_refresh_chroot: bool,

        /// Changelog urgency (default medium; e.g. high for security).
        #[arg(
            long,
            value_name = "LEVEL",
            default_value = "medium",
            help_heading = "Stages"
        )]
        urgency: String,

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

        /// Upload stage: Debusine repo owner (uploads to r-NAME-<pkg>).
        #[arg(
            long,
            value_name = "NAME",
            help_heading = "Upload",
            conflicts_with_all = ["ppa", "upload_target"],
            long_help = "\
Upload stage: publish to a Debusine personal repository instead of a
dput archive. NAME is the repository owner (the r-NAME-* workspace
prefix on debusine.debian.net); dbranch uploads with
  dput -O debusine_workspace=r-NAME-<srcpkg>
       -O debusine_workflow=publish-to-<suite>-<srcpkg>
(--debusine-project replaces <srcpkg> for shared workspaces)
where <suite> is the target's base release (a trixie backport
publishes to trixie). Debian targets only. Needs debusine-client and
a `debusine setup` token.
See wiki.debian.org/DebusineDebianNet#Repositories."
        )]
        debusine: Option<String>,

        /// Upload stage: Debusine project name (default: source pkg).
        #[arg(
            long,
            value_name = "PROJECT",
            help_heading = "Upload",
            requires = "debusine",
            long_help = "\
Upload stage: the Debusine project name — the part after the owner
in the r-NAME-PROJECT workspace and publish-to-<suite>-PROJECT
workflow. Defaults to the source package name, which fits a repo
shipping one package; a shared workspace hosting several packages
names its project here. Requires --debusine."
        )]
        debusine_project: Option<String>,

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

    /// Update the Debian branch to a new upstream release.
    Update {
        /// Debian branch to update (defaults to the current branch).
        #[arg(value_name = "BRANCH")]
        branch: Option<String>,

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
  import  gbp import-orig --uscan + gbp dch -c -R; or,
          when packaging from upstream's git (see
          `clone`), merge its release tag instead
  source  debuild -S the source package (-si for the
          Debian archive, -sa for anywhere else)
  build   pbuilder-dist scratch build of the .dsc
  lint    lintian on the built source package
  push    git push the branch, then watch its CI
  upload  dput the built package
  tag     dh clean + gbp tag the release
  all     import + source + build + lint + push
Defaults to `import`."
        )]
        stage: Vec<String>,

        /// Debian suite to build against (e.g. `unstable`).
        #[arg(
            long,
            value_name = "SUITE",
            default_value = "testing",
            help_heading = "Stages"
        )]
        build_suite: String,

        /// Remote holding upstream's git, to merge releases from
        /// (default: one named `upstream`, if present).
        #[arg(long, value_name = "NAME", help_heading = "Stages")]
        upstream_remote: Option<String>,

        /// Upstream release to merge in (default: the newest tag).
        #[arg(long, value_name = "VERSION", help_heading = "Stages")]
        upstream_version: Option<String>,

        /// In the push stage, push but don't wait for / watch CI.
        #[arg(long, help_heading = "Stages")]
        nowait: bool,

        /// Answer yes to prompts (e.g. create a missing Debusine
        /// workspace) instead of asking.
        #[arg(short = 'y', long)]
        yes: bool,

        /// Build stage: force-refresh the pbuilder chroot first.
        #[arg(long, help_heading = "Stages", conflicts_with = "no_refresh_chroot")]
        refresh_chroot: bool,

        /// Build stage: never auto-refresh the pbuilder chroot.
        #[arg(long, help_heading = "Stages")]
        no_refresh_chroot: bool,

        /// Changelog urgency (default medium; e.g. high for security).
        #[arg(
            long,
            value_name = "LEVEL",
            default_value = "medium",
            help_heading = "Stages"
        )]
        urgency: String,

        /// Upload stage: dput target (default: dput's own; e.g. mentors).
        #[arg(long, value_name = "TARGET", help_heading = "Upload")]
        upload_target: Option<String>,

        /// Upload stage: Debusine repo owner (uploads to r-NAME-<pkg>).
        #[arg(
            long,
            value_name = "NAME",
            help_heading = "Upload",
            conflicts_with = "upload_target",
            long_help = "\
Upload stage: publish to a Debusine personal repository instead of a
dput archive. NAME is the repository owner (the r-NAME-* workspace
prefix on debusine.debian.net); dbranch uploads with
  dput -O debusine_workspace=r-NAME-<srcpkg>
       -O debusine_workflow=publish-to-sid-<srcpkg>
(--debusine-project replaces <srcpkg> for shared workspaces; the
Debian branch targets unstable, whose Debusine suite is sid).
Needs debusine-client and a `debusine setup` token.
See wiki.debian.org/DebusineDebianNet#Repositories."
        )]
        debusine: Option<String>,

        /// Upload stage: Debusine project name (default: source pkg).
        #[arg(
            long,
            value_name = "PROJECT",
            help_heading = "Upload",
            requires = "debusine",
            long_help = "\
Upload stage: the Debusine project name — the part after the owner
in the r-NAME-PROJECT workspace and publish-to-sid-PROJECT
workflow. Defaults to the source package name, which fits a repo
shipping one package; a shared workspace hosting several packages
names its project here. Requires --debusine."
        )]
        debusine_project: Option<String>,

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
    let cli = sandogasa_cli::parse_with_defaults::<Cli>(env!("CARGO_PKG_NAME"));
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
            remote,
            nowait,
            refresh_chroot,
            no_refresh_chroot,
            urgency,
            yes,
            include_eol,
            ppa,
            upload_target,
            debusine,
            debusine_project,
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
                debusine,
                debusine_project,
                source,
                remote,
                chroot_refresh,
                assume_yes: yes,
                include_eol,
                urgency,
            };
            rebuild::run(&ui, &repo, &opts)
        }
        Command::Clone {
            url,
            dir,
            upstream_version,
            debian_branch,
            upstream_remote,
            from_upstream_packaging,
            fresh,
            salsa,
            mr,
            mrconfig,
            dry_run,
            explain,
            quiet,
        } => {
            let ui = Ui {
                explain,
                dry_run,
                quiet,
            };
            let packaging = match (from_upstream_packaging, fresh) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            };
            upstream::clone(
                &ui,
                &CloneOptions {
                    url,
                    dir,
                    upstream_version,
                    debian_branch,
                    upstream_remote,
                    packaging,
                    salsa,
                    mr,
                    mrconfig,
                },
            )
        }
        Command::Update {
            branch,
            repo,
            stage,
            build_suite,
            upstream_remote,
            upstream_version,
            nowait,
            yes,
            refresh_chroot,
            no_refresh_chroot,
            urgency,
            upload_target,
            debusine,
            debusine_project,
            dry_run,
            explain,
            quiet,
        } => {
            let ui = Ui {
                explain,
                dry_run,
                quiet,
            };
            let stages = rebuild::parse_update_stages(&stage)?;
            let chroot_refresh = if refresh_chroot {
                ChrootRefresh::Force
            } else if no_refresh_chroot {
                ChrootRefresh::Never
            } else {
                ChrootRefresh::Auto
            };
            let opts = UpdateOptions {
                branch,
                stages,
                build_suite,
                upstream_remote,
                upstream_version,
                nowait,
                upload_target,
                debusine,
                debusine_project,
                chroot_refresh,
                urgency,
                assume_yes: yes,
            };
            rebuild::update(&ui, &repo, &opts)
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

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/dbranch.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }
}
