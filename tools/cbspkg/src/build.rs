// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `build`: build the checked-out SIG branch in CBS.
//!
//! By hand that is `cbs build <target> git+https://<host>/<SIG
//! group>/<pkg>.git#<full commit hash>`: a target spelled after the
//! collection, and a commit hash from `git rev-parse` that has to be in
//! the SIG's repo, where CBS fetches it. All of it follows from the
//! branch, so it is worked out here and shown as the commands it comes
//! to. The commit has to be pushed first, and a real (not scratch) build
//! needs the package listed in the collection's tags, which `add-pkg`'s
//! check does on the way. Like every write to CBS, the build is
//! confirmed at a terminal and cannot be scripted past.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sandogasa_cbs::packages::latest;
use sandogasa_cbs::remotes::Role;
use sandogasa_cbs::{Collection, Stage};
use sandogasa_cli::explain::Ui;

use crate::add_pkg::{Checkout, list_missing, resolve};

#[derive(clap::Args)]
pub struct BuildArgs {
    /// The branch to build (default: the one checked out)
    #[arg(long, value_name = "BRANCH", help_heading = "Selection")]
    pub branch: Option<String>,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR", help_heading = "Selection")]
    pub repo: Option<PathBuf>,

    #[command(flatten, next_help_heading = "Selection")]
    pub variants: crate::add_pkg::VariantArgs,

    /// Build at a lower priority (cbs build --background)
    #[arg(long, help_heading = "Building")]
    pub background: bool,

    /// Start the builds and leave them running, printing the command
    /// that waits for them
    #[arg(long, help_heading = "Building")]
    pub nowait: bool,

    /// The owner to list a new package under (default: your CBS login)
    #[arg(long, value_name = "USER", help_heading = "Building")]
    pub owner: Option<String>,

    /// Build without keeping the result (no tags, no listing needed)
    #[arg(long, help_heading = "Building")]
    pub scratch: bool,

    /// Show each step and its command without running anything
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long, help_heading = "Output")]
    pub explain: bool,
}

/// How a build is started: `cbs build`'s flags beyond the target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildFlags {
    pub scratch: bool,
    pub background: bool,
    /// Return once the task is created (`--nowait`), for building
    /// several at once and waiting on them together.
    pub nowait: bool,
}

/// Where a checkout's branch is to be built from: the SIG's remote
/// (`name`, URL), its project path, and the branch's commit.
pub struct Located {
    pub remote: String,
    pub url: String,
    pub sig_path: String,
    pub commit: String,
}

/// The host of a git remote URL: `gitlab.com` for
/// `git+ssh://git@gitlab.com/…` or `git@gitlab.com:…`.
pub fn remote_host(url: &str) -> Option<String> {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest.split('/').next()?,
        None => url.split(':').next()?,
    };
    let host = rest.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// What CBS builds from: the SIG repo at `path` on `host`, at `commit`.
pub fn scm_url(host: &str, path: &str, commit: &str) -> String {
    format!("git+https://{host}/{path}.git#{commit}")
}

/// The command that builds `scm` in `target`, as a person would type it.
pub fn build_argv(target: &str, scm: &str, flags: BuildFlags) -> Vec<String> {
    let mut argv = vec!["cbs".to_string(), "build".to_string()];
    for (on, flag) in [
        (flags.scratch, "--scratch"),
        (flags.background, "--background"),
        (flags.nowait, "--nowait"),
    ] {
        if on {
            argv.push(flag.to_string());
        }
    }
    argv.extend([target.to_string(), scm.to_string()]);
    argv
}

/// The task a `cbs build --nowait` started, from its `Created task: N`
/// line.
pub fn created_task(output: &str) -> Option<u64> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("Created task:")?.trim().parse().ok())
}

/// The command that waits for `tasks`, as a person would type it.
pub fn watch_argv(tasks: &[String]) -> Vec<String> {
    ["cbs", "watch-task"]
        .into_iter()
        .map(String::from)
        .chain(tasks.iter().cloned())
        .collect()
}

pub fn run(args: &BuildArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run `git` in `repo` to read something, its output trimmed.
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Find the SIG's remote and the branch's commit: the branch itself or,
/// when it is only on the SIG's remote (after `sig-repo` made no local
/// branch), the remote's.
pub fn locate(checkout: &Checkout, repo: &Path) -> Result<Located, Box<dyn std::error::Error>> {
    let (branch, package) = (&checkout.branch, &checkout.package);
    let sig = checkout.sig;
    let sig_path = format!("{}/{package}", sig.gitlab_group());
    let found = checkout.remotes.find(Role::Sig(sig)).ok_or_else(|| {
        format!(
            "no remote points at the SIG's repo {sig_path}, which CBS builds from: \
             `cbspkg sig-repo` sets it up, or add it with `git remote add {} \
             git+ssh://git@gitlab.com/{sig_path}.git`",
            sandogasa_cbs::remotes::name_for(Role::Sig(sig))
        )
    })?;
    let (remote, url) = (found.name.clone(), found.url.clone());
    let commit = git(
        repo,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )
    .or_else(|_| {
        git(
            repo,
            &[
                "rev-parse",
                "--verify",
                &format!("{remote}/{branch}^{{commit}}"),
            ],
        )
    })
    .map_err(|_| format!("neither {branch} nor {remote}/{branch} is a branch here"))?;
    Ok(Located {
        remote,
        url,
        sig_path,
        commit,
    })
}

fn run_inner(args: &BuildArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut tools = vec![
        ("git", "sudo dnf install git-core", Some("--version")),
        ("koji", "sudo dnf install koji", Some("--help")),
    ];
    if !args.dry_run {
        tools.push(("cbs", "sudo dnf install centos-packager", Some("--help")));
    }
    sandogasa_cli::require_tools(&tools)?;
    let checkout = resolve(&repo, args.branch.as_deref(), None, Some(&args.variants))?;
    let (branch, package) = (&checkout.branch, &checkout.package);
    let Located {
        remote,
        url,
        sig_path,
        commit,
    } = locate(&checkout, &repo)?;
    let host = remote_host(&url).ok_or_else(|| format!("cannot tell the host of {url}"))?;

    // Whether the SIG's repo has the commit: CBS fetches it from there.
    // Reading changes nothing, so it is done even in a dry run.
    ui.step(&format!(
        "Find the commit to build, and check {sig_path} has it"
    ));
    let show = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    ui.show_command(&show(&["git", "rev-parse", branch]));
    ui.show_command(&show(&["git", "fetch", &remote]));
    git(&repo, &["fetch", "--quiet", &remote])?;
    let containing = git(
        &repo,
        &[
            "branch",
            "--remotes",
            "--contains",
            &commit,
            "--list",
            &format!("{remote}/*"),
        ],
    )?;
    if containing.trim().is_empty() {
        return Err(format!(
            "{branch} is at {}, which {sig_path} does not have yet; push it first:\n  \
             git push {remote} {branch}",
            &commit[..12.min(commit.len())]
        )
        .into());
    }
    let scm = scm_url(&host, &sig_path, &commit);

    // CBS fetches the sources from the SIG lookaside, scratch builds
    // too (#99).
    if !crate::upload_sources::upload_missing(&ui, &repo, &checkout, &remote, &commit)? {
        return Ok(());
    }
    if !args.scratch
        && !list_missing(
            &ui,
            &repo,
            &checkout.collections,
            package,
            args.owner.as_deref(),
        )?
    {
        return Ok(());
    }
    // Several builds run at once: each started with --nowait, then
    // waited on together. One waits as it runs, unless asked not to.
    let together = checkout.collections.len() > 1 || args.nowait;
    let flags = BuildFlags {
        scratch: args.scratch,
        background: args.background,
        nowait: together,
    };
    let builds: Vec<(&Collection, Vec<String>)> = checkout
        .collections
        .iter()
        .map(|c| (c, build_argv(&c.target(), &scm, flags)))
        .collect();
    let kind = if args.scratch {
        "a scratch build"
    } else {
        "a build"
    };
    if !args.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, args.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err("building in CBS is confirmed at a terminal; run it in one".into());
        }
        // The exact commands, whatever the narration: what would
        // otherwise be pieced together from shell history.
        // Set apart from the login check and the steps above.
        println!(
            "\n{package} {branch} at {}:",
            &commit[..12.min(commit.len())]
        );
        for (_, argv) in &builds {
            println!("  {}", sandogasa_cli::explain::command_line(argv));
        }
        if together && !args.nowait {
            println!(
                "  {}",
                sandogasa_cli::explain::command_line(&watch_argv(&["<tasks>".into()]))
            );
        }
        if !sandogasa_cli::confirm(&format!("Start {kind}?"), true)? {
            println!("Nothing built.");
            return Ok(());
        }
    }
    let mut tasks = Vec::new();
    for (collection, argv) in &builds {
        ui.step(&format!(
            "Start {kind} of {package} in {}, which puts it in {}",
            collection.target(),
            if args.scratch {
                "no tag".to_string()
            } else {
                collection.tag(Stage::Candidate)
            }
        ));
        if !together {
            ui.run_required(argv, &repo)?;
            continue;
        }
        let (code, output) = ui.run_capture(argv, &repo)?;
        print!("{output}");
        if code != 0 {
            // The builds started already keep running: say how to
            // follow them.
            if !tasks.is_empty() {
                println!(
                    "\nThe builds started already are running; to wait for them: {}",
                    sandogasa_cli::explain::command_line(&watch_argv(&tasks))
                );
            }
            return Err(format!("{} failed (exit {code})", argv.join(" ")).into());
        }
        if !args.dry_run {
            let task = created_task(&output)
                .ok_or_else(|| format!("cbs build printed no task for {}", collection.target()))?;
            tasks.push(task.to_string());
        }
    }
    if together {
        let shown = if args.dry_run {
            vec!["<tasks>".to_string()]
        } else {
            tasks.clone()
        };
        let watch = watch_argv(&shown);
        if args.nowait {
            println!(
                "\nThe builds are running; to wait for them: {}",
                sandogasa_cli::explain::command_line(&watch)
            );
            return Ok(());
        }
        ui.step("Wait for the builds, which run at the same time");
        ui.run_required(&watch, &repo)?;
    }
    if !args.scratch && !args.dry_run {
        // Set apart from the build's own output, which ends just above.
        println!();
        for collection in &checkout.collections {
            if let Some(nvr) = latest(&collection.tag(Stage::Candidate), package) {
                println!(
                    "Next, to offer {nvr} for testing: cbspkg promote (cbs tag-build {} {nvr})",
                    collection.tag(Stage::Testing)
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_read_from_either_kind_of_remote_url() {
        for url in [
            "git+ssh://git@gitlab.com/CentOS/proposed_updates/rpms/rsync.git",
            "git@gitlab.com:CentOS/proposed_updates/rpms/rsync.git",
            "https://gitlab.com/CentOS/proposed_updates/rpms/rsync",
            "ssh://git@gitlab.com:22/CentOS/x.git",
        ] {
            assert_eq!(remote_host(url).as_deref(), Some("gitlab.com"), "{url}");
        }
    }

    #[test]
    fn the_build_command_names_the_target_and_the_sig_repos_commit() {
        let scm = scm_url(
            "gitlab.com",
            "CentOS/proposed_updates/rpms/rsync",
            "e4441b13be38e220451c8186abca5b27c44cd651",
        );
        assert_eq!(
            build_argv(
                "proposed_updates10s-packages-main-el10s",
                &scm,
                BuildFlags::default()
            )
            .join(" "),
            "cbs build proposed_updates10s-packages-main-el10s \
             git+https://gitlab.com/CentOS/proposed_updates/rpms/rsync.git\
             #e4441b13be38e220451c8186abca5b27c44cd651"
        );
        let scratch = BuildFlags {
            scratch: true,
            ..Default::default()
        };
        assert_eq!(
            build_argv("t", "s", scratch),
            ["cbs", "build", "--scratch", "t", "s"]
        );
        let all = BuildFlags {
            scratch: true,
            background: true,
            nowait: true,
        };
        assert_eq!(
            build_argv("t", "s", all),
            [
                "cbs",
                "build",
                "--scratch",
                "--background",
                "--nowait",
                "t",
                "s"
            ]
        );
    }

    /// What `cbs build --nowait` printed for perf on 2026-11-10's
    /// pattern, and the command that then waits for both builds.
    #[test]
    fn several_builds_are_started_then_waited_on_together() {
        let out = "Created task: 5531001\nTask info: https://cbs.centos.org/koji/taskinfo?taskID=5531001\n";
        assert_eq!(created_task(out), Some(5531001));
        assert_eq!(created_task("Uploading srpm\n"), None);
        assert_eq!(
            watch_argv(&["5531001".into(), "5531002".into()]),
            ["cbs", "watch-task", "5531001", "5531002"]
        );
    }
}
