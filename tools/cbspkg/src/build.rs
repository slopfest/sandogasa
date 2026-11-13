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

/// The builds made into each of `collections`, by the task started for
/// it (`tasks`, in the same order), which names it exactly; without
/// tasks, the newest in each `-candidate`.
fn builds_of<'a>(
    collections: &'a [Collection],
    tasks: &[String],
    package: &str,
) -> Vec<(&'a Collection, String)> {
    collections
        .iter()
        .enumerate()
        .filter_map(|(n, collection)| {
            let nvr = match tasks.get(n) {
                Some(task) => sandogasa_cbs::packages::task_build(task)
                    .map_err(|e| eprintln!("warning: cannot read task {task}'s build ({e})"))
                    .ok()
                    .flatten(),
                None if tasks.is_empty() => latest(&collection.tag(Stage::Candidate), package),
                None => None,
            }?;
            Some((collection, nvr))
        })
        .collect()
}

/// Tell the tracking issue of `built`, the builds just made from
/// `commit`: planned again with their NVRs, so what the issue was told
/// already is checked against them, and the comments name them.
fn tell_built(
    base: &str,
    token: Option<&str>,
    on: &crate::notify::Branch,
    commit: &str,
    built: &[(&Collection, String)],
) {
    if built.is_empty() {
        return;
    }
    let steps: Vec<(String, Stage, Vec<Vec<String>>)> = built
        .iter()
        .map(|(c, nvr)| {
            let tag = c.tag(Stage::Candidate);
            let argv = ["cbs", "tag-build", &tag, nvr].map(String::from).to_vec();
            (nvr.clone(), Stage::Candidate, vec![argv])
        })
        .collect();
    let notices = crate::notify::plan(base, token, on, &steps, &|_| Ok(Some(commit.to_string())));
    let done: Vec<(String, Stage)> = built
        .iter()
        .map(|(_, nvr)| (nvr.clone(), Stage::Candidate))
        .collect();
    crate::notify::post(base, token, &notices, &done, &crate::notify::cbs_latest);
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
        // Only on the SIG's remote: fetch first, so the commit is the
        // branch's now, not where the remote-tracking ref last was.
        git(repo, &["fetch", "--quiet", &remote])?;
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
    // The tracking issue is told of a build cbspkg waits for, which it
    // knows by its task.
    let tells = !args.scratch && !args.nowait && crate::notify::covers(checkout.sig);
    // Several builds run at once: each started with --nowait, then
    // waited on together. One waits as it runs, unless asked not to, or
    // its task is wanted for its NVR.
    let together = checkout.collections.len() > 1 || args.nowait || tells;
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
    // The cheap checks first: the CBS login and a terminal to confirm
    // at, before any GitLab request.
    if !args.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, args.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err("building in CBS is confirmed at a terminal; run it in one".into());
        }
    }
    // What the tracking issue will be told, shown with what is
    // confirmed: with a stand-in for each build's NVR, until its task
    // names it.
    let base = crate::config::gitlab_base();
    let token = crate::config::gitlab_token();
    let stand_ins: Vec<String> = checkout
        .collections
        .iter()
        .map(|c| format!("<the build in {}>", c.prefix()))
        .collect();
    let preview = if tells {
        ui.step("Find the tracking issue to update, through GitLab's API");
        let steps: Vec<(String, Stage, Vec<Vec<String>>)> = checkout
            .collections
            .iter()
            .zip(&stand_ins)
            .map(|(c, nvr)| {
                let tag = c.tag(Stage::Candidate);
                let argv = ["cbs", "tag-build", &tag, nvr].map(String::from).to_vec();
                (nvr.clone(), Stage::Candidate, vec![argv])
            })
            .collect();
        crate::notify::plan(
            &base,
            token.as_deref(),
            &crate::notify::Branch {
                sig: checkout.sig,
                package,
                branch,
                repo: &repo,
                dry_run: args.dry_run,
            },
            &steps,
            &|_| Ok(Some(commit.clone())),
        )
    } else {
        Vec::new()
    };
    if args.nowait && !args.scratch && crate::notify::covers(checkout.sig) {
        println!(
            "Builds left running get no comment on the tracking issue: cbspkg is not there when they end."
        );
    }
    if !args.dry_run {
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
        for notice in &preview {
            print!("{}", crate::notify::describe(notice, token.is_some()));
        }
        if !sandogasa_cli::confirm(&format!("Start {kind}?"), true)? {
            println!("Nothing built.");
            return Ok(());
        }
    }
    let mut tasks: Vec<String> = Vec::new();
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
        // Whatever stops the run here, the builds started already keep
        // running: say how to follow them.
        let started = |tasks: &[String]| {
            if !tasks.is_empty() {
                println!(
                    "\nThe builds started already are running; to wait for them: {}",
                    sandogasa_cli::explain::command_line(&watch_argv(tasks))
                );
            }
        };
        let (code, output) = match ui.run_capture(argv, &repo) {
            Ok(r) => r,
            Err(e) => {
                started(&tasks);
                return Err(e.into());
            }
        };
        print!("{output}");
        if code != 0 {
            started(&tasks);
            return Err(format!("{} failed (exit {code})", argv.join(" ")).into());
        }
        if !args.dry_run {
            let Some(task) = created_task(&output) else {
                started(&tasks);
                return Err(
                    format!("cbs build printed no task for {}", collection.target()).into(),
                );
            };
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
    if args.dry_run {
        // After the steps, where a real run tells the issue.
        for notice in &preview {
            ui.step("Comment on the tracking issue, through GitLab's API");
            print!("{}", crate::notify::describe(notice, token.is_some()));
        }
    }
    if !args.scratch && !args.dry_run {
        // Set apart from the build's own output, which ends just above.
        println!();
        let built = builds_of(&checkout.collections, &tasks, package);
        for (collection, nvr) in &built {
            println!(
                "Next, to offer {nvr} for testing: cbspkg promote (cbs tag-build {} {nvr})",
                collection.tag(Stage::Testing)
            );
        }
        if tells {
            let on = crate::notify::Branch {
                sig: checkout.sig,
                package,
                branch,
                repo: &repo,
                dry_run: false,
            };
            tell_built(&base, token.as_deref(), &on, &commit, &built);
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

    use crate::test_support::Fixture;

    fn dry_run(branch: Option<&str>, repo: &Fixture) -> BuildArgs {
        BuildArgs {
            branch: branch.map(str::to_string),
            repo: Some(repo.repo.clone()),
            variants: Default::default(),
            background: false,
            nowait: false,
            owner: None,
            scratch: false,
            dry_run: true,
            explain: false,
        }
    }

    /// koji answering for a package listed in every tag.
    const LISTED: &[(&str, &str)] = &[
        ("--help", ""),
        (
            "list-pkgs",
            "rsync  proposed_updates10s-packages-main-candidate  salimma",
        ),
    ];

    #[test]
    #[serial_test::serial]
    fn a_dry_run_scratch_build_shows_the_build_without_listing_or_running_it() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        // koji only answers --help: a scratch build asks it nothing else.
        let args = BuildArgs {
            scratch: true,
            background: true,
            ..dry_run(None, &f)
        };
        run_inner(&args).unwrap();
        assert_eq!(run(&args), ExitCode::SUCCESS);
    }

    #[test]
    #[serial_test::serial]
    fn a_dry_run_build_checks_the_tags_list_the_package() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.fake("koji", LISTED);
        run_inner(&dry_run(None, &f)).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn a_dry_run_build_of_an_unlisted_package_lists_it_for_the_cbs_user() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.fake(
            "koji",
            &[
                ("--help", ""),
                ("list-pkgs", "!(no matching packages)"),
                ("moshimoshi", "what's up, tester!"),
            ],
        );
        run_inner(&dry_run(None, &f)).unwrap();
        // An owner given is used as it is, the CBS user not asked.
        f.fake(
            "koji",
            &[("--help", ""), ("list-pkgs", "!(no matching packages)")],
        );
        let args = BuildArgs {
            owner: Some("someone".into()),
            ..dry_run(None, &f)
        };
        run_inner(&args).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn a_failed_listing_check_stops_the_build() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.fake(
            "koji",
            &[("--help", ""), ("list-pkgs", "!GenericError: no such tag")],
        );
        let err = run_inner(&dry_run(None, &f)).unwrap_err().to_string();
        assert!(err.contains("no such tag"), "{err}");
    }

    /// perf on c9s-hs+fb builds for Stream and RHEL at once: started
    /// with --nowait and waited on together.
    #[test]
    #[serial_test::serial]
    fn a_dry_run_build_into_several_collections_waits_on_them_together() {
        let f = Fixture::hyperscale("perf", "c9s-hs+fb");
        let args = BuildArgs {
            scratch: true,
            ..dry_run(None, &f)
        };
        run_inner(&args).unwrap();
        let args = BuildArgs {
            nowait: true,
            ..args
        };
        run_inner(&args).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn a_commit_the_sigs_repo_lacks_is_to_be_pushed_first() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        std::fs::write(f.repo.join("rsync.spec"), "Name: y\n").unwrap();
        f.git(&["commit", "--quiet", "-am", "unpushed"]);
        let err = run_inner(&dry_run(None, &f)).unwrap_err().to_string();
        assert!(
            err.contains("does not have yet") && err.contains("git push sig c10s-proposed-RHEL-1"),
            "{err}"
        );
    }

    /// After sig-repo pushed a branch without making a local one.
    #[test]
    #[serial_test::serial]
    fn a_branch_only_on_the_sigs_remote_is_built_from_there() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.git(&[
            "push",
            "--quiet",
            "sig",
            "HEAD:refs/heads/c10s-proposed-RHEL-2",
        ]);
        let checkout =
            crate::add_pkg::resolve(&f.repo, Some("c10s-proposed-RHEL-2"), None, None).unwrap();
        let located = locate(&checkout, &f.repo).unwrap();
        assert_eq!(located.remote, "sig");
        assert_eq!(located.sig_path, "CentOS/proposed_updates/rpms/rsync");
        assert_eq!(located.commit, f.git(&["rev-parse", "HEAD"]));
        let args = BuildArgs {
            scratch: true,
            ..dry_run(Some("c10s-proposed-RHEL-2"), &f)
        };
        run_inner(&args).unwrap();
        let err = run_inner(&dry_run(Some("c10s-proposed-RHEL-3"), &f))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("neither c10s-proposed-RHEL-3 nor sig/c10s-proposed-RHEL-3"),
            "{err}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_checkout_without_the_sigs_repo_says_how_to_add_it() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.git(&["remote", "remove", "sig"]);
        f.git(&[
            "remote",
            "add",
            "origin",
            "git@gitlab.com:redhat/centos-stream/rpms/rsync.git",
        ]);
        let err = run_inner(&dry_run(None, &f)).unwrap_err().to_string();
        assert!(
            err.contains("no remote points at the SIG's repo")
                && err.contains("git remote add proposed_updates"),
            "{err}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_build_needs_koji_and_cbs() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.fake("koji", &[]);
        assert_eq!(run(&dry_run(None, &f)), ExitCode::FAILURE);
        // A real build needs cbs as well, checked before anything else.
        f.fake("koji", &[("--help", "")]);
        f.fake("cbs", &[]);
        let args = BuildArgs {
            dry_run: false,
            ..dry_run(None, &f)
        };
        let err = run_inner(&args).unwrap_err().to_string();
        assert!(err.contains("cbs (install"), "{err}");
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

    /// A Hyperscale build of ethtool 7.1, named by its task: the issue
    /// asking for 7.1 is told, once, with the build's NVR.
    #[test]
    #[serial_test::serial]
    fn a_finished_build_is_named_by_its_task_and_told() {
        let f = Fixture::hyperscale("ethtool", "c9s-hs");
        f.fake(
            "koji",
            &[
                ("taskinfo 101", "Task: 101\nBuild: ethtool-7.1-1.hs.el9 (5)"),
                ("taskinfo", "!GenericError: No such task"),
                ("list-tagged", "ethtool-7.0-1.hs.el9  t  someone"),
            ],
        );
        let checkout = resolve(&f.repo, None, None, None).unwrap();
        let tasks = vec!["101".to_string()];
        let built = builds_of(&checkout.collections, &tasks, "ethtool");
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].1, "ethtool-7.1-1.hs.el9");
        // No task: the newest in -candidate.
        let newest = builds_of(&checkout.collections, &[], "ethtool");
        assert_eq!(newest[0].1, "ethtool-7.0-1.hs.el9");
        // A task that names no build: nothing.
        assert!(builds_of(&checkout.collections, &["102".to_string()], "ethtool").is_empty());

        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fethtool";
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                r#"[{"iid": 2, "title": "ethtool-7.1 is available", "state": "opened",
                     "web_url": "h2", "labels": ["rfe::new-version"]}]"#,
            )
            .create();
        server
            .mock("GET", format!("{path}/issues/2/notes").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create();
        let comment = server
            .mock("POST", format!("{path}/issues/2/notes").as_str())
            .match_body(mockito::Matcher::Regex(
                "ethtool-7.1-1.hs.el9` is built: tagged into".into(),
            ))
            .with_status(201)
            .with_body(r#"{"id": 1, "body": "b"}"#)
            .expect(1)
            .create();
        server
            .mock("POST", "/api/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"project": {"workItems": {"nodes": []}}}}"#)
            .create();
        let on = crate::notify::Branch {
            sig: checkout.sig,
            package: "ethtool",
            branch: "c9s-hs",
            repo: &f.repo,
            dry_run: false,
        };
        tell_built(&server.url(), Some("t"), &on, "abc123", &built);
        comment.assert();
        tell_built(&server.url(), Some("t"), &on, "abc123", &[]);
    }
}
