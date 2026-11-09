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
use std::process::{Command, ExitCode, Stdio};

use sandogasa_cbs::{Collection, PROFILE, Sig, Stage};
use sandogasa_cli::explain::Ui;

use crate::add_pkg::{list_missing, resolve};

#[derive(clap::Args)]
pub struct BuildArgs {
    /// The branch to build (default: the one checked out)
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long)]
    pub explain: bool,

    /// The owner to list a new package under (default: your CBS login)
    #[arg(long, value_name = "USER")]
    pub owner: Option<String>,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    /// Build without keeping the result (no tags, no listing needed)
    #[arg(long)]
    pub scratch: bool,
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
pub fn build_argv(target: &str, scm: &str, scratch: bool) -> Vec<String> {
    let mut argv = vec!["cbs".to_string(), "build".to_string()];
    if scratch {
        argv.push("--scratch".to_string());
    }
    argv.extend([target.to_string(), scm.to_string()]);
    argv
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

/// The newest build of `package` in `tag`, as koji lists it.
fn latest_in(tag: &str, package: &str) -> Option<String> {
    let out = Command::new("koji")
        .args([
            "--profile",
            PROFILE,
            "list-tagged",
            "--latest",
            "--quiet",
            tag,
            package,
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
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
    let checkout = resolve(&repo, args.branch.as_deref(), None)?;
    let (branch, package) = (&checkout.branch, &checkout.package);
    let sig = Sig::ALL
        .into_iter()
        .find(|s| s.name() == checkout.collections[0].sig)
        .ok_or("the branch's SIG has no known GitLab group")?;
    let sig_path = format!("{}/{package}", sig.gitlab_group());
    let (remote, url) = checkout
        .remotes
        .iter()
        .find(|(_, u)| {
            crate::sig_repo::project_path(u).is_some_and(|p| p.eq_ignore_ascii_case(&sig_path))
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "no remote points at the SIG's repo {sig_path}, which CBS builds from: \
                 `cbspkg sig-repo` sets it up, or add it with `git remote add sig \
                 git+ssh://git@gitlab.com/{sig_path}.git`"
            )
        })?;
    let host = remote_host(&url).ok_or_else(|| format!("cannot tell the host of {url}"))?;

    // The commit, and whether the SIG's repo has it: CBS fetches it
    // from there. Reading both changes nothing, so it is done even in
    // a dry run.
    // The branch itself, or, when it is only on the SIG's remote (as
    // after `sig-repo` without a local branch), the remote's.
    let commit = git(
        &repo,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )
    .or_else(|_| {
        git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("{remote}/{branch}^{{commit}}"),
            ],
        )
    })
    .map_err(|_| format!("neither {branch} nor {remote}/{branch} is a branch here"))?;
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
    let targets: Vec<String> = checkout
        .collections
        .iter()
        .map(Collection::target)
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
        println!(
            "{package} {branch} at {}\n  from {scm}\n  in {}",
            &commit[..12.min(commit.len())],
            targets.join(", ")
        );
        if !sandogasa_cli::confirm(&format!("Start {kind}?"), true)? {
            println!("Nothing built.");
            return Ok(());
        }
    }
    for (collection, target) in checkout.collections.iter().zip(&targets) {
        ui.step(&format!(
            "Start {kind} of {package} in {target}, which puts it in {}",
            if args.scratch {
                "no tag".to_string()
            } else {
                collection.tag(Stage::Candidate)
            }
        ));
        ui.run_required(&build_argv(target, &scm, args.scratch), &repo)?;
    }
    if !args.scratch && !args.dry_run {
        for collection in &checkout.collections {
            if let Some(nvr) = latest_in(&collection.tag(Stage::Candidate), package) {
                println!(
                    "Next, to offer it for testing: cbs tag-build {} {nvr}",
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
            build_argv("proposed_updates10s-packages-main-el10s", &scm, false).join(" "),
            "cbs build proposed_updates10s-packages-main-el10s \
             git+https://gitlab.com/CentOS/proposed_updates/rpms/rsync.git\
             #e4441b13be38e220451c8186abca5b27c44cd651"
        );
        assert_eq!(
            build_argv("t", "s", true),
            ["cbs", "build", "--scratch", "t", "s"]
        );
    }
}
