// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `add-pkg`: list a package in the CBS tags of the collection its
//! branch builds into, so that its first build is accepted (#97).
//!
//! CBS builds a package into a tag only once the tag lists it, with an
//! owner, and a build moves through three tags (`-candidate`,
//! `-testing`, `-release`), so a first build needs three
//! `cbs add-pkg --owner=<user> <tag> <package>` commands, with tag names
//! that are easy to get wrong. The collection comes from the branch
//! (sandogasa-cbs), the owner from the CBS login, and only the tags
//! that do not list the package yet are changed. Like every write to
//! CBS, it is confirmed at a terminal and cannot be scripted past.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sandogasa_cbs::packages::{add_pkg_argv, listed, stage_tags, whoami};
use sandogasa_cbs::{Collection, Sig};
use sandogasa_cli::explain::Ui;

#[derive(clap::Args)]
pub struct AddPkgArgs {
    /// The package (default: the one the checkout's remotes are for)
    pub package: Option<String>,

    /// The branch whose collection to list it in (default: the one
    /// checked out)
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long)]
    pub explain: bool,

    /// The package's owner in CBS (default: your CBS login)
    #[arg(long, value_name = "USER")]
    pub owner: Option<String>,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,
}

/// The package a remote URL's project is: `rsync` for Stream's or a
/// SIG's repo of it.
pub fn package_of_remote(url: &str) -> Option<String> {
    let path = crate::sig_repo::project_path(url)?;
    let (group, package) = path.rsplit_once('/')?;
    let known = group.eq_ignore_ascii_case("redhat/centos-stream/rpms")
        || Sig::ALL
            .iter()
            .any(|s| group.eq_ignore_ascii_case(s.gitlab_group()));
    (known && !package.is_empty()).then(|| package.to_string())
}

/// The SIGs whose repos the checkout's remotes (their URLs) point at.
pub fn sigs_of_remotes<'a>(urls: impl IntoIterator<Item = &'a str>) -> Vec<Sig> {
    let mut sigs: Vec<Sig> = Vec::new();
    for url in urls {
        let Some(path) = crate::sig_repo::project_path(url) else {
            continue;
        };
        let Some((group, _)) = path.rsplit_once('/') else {
            continue;
        };
        if let Some(sig) = Sig::ALL
            .into_iter()
            .find(|s| group.eq_ignore_ascii_case(s.gitlab_group()))
            && !sigs.contains(&sig)
        {
            sigs.push(sig);
        }
    }
    sigs
}

/// The CentOS Stream major a branch starts from: 10 for `c10s` or
/// `c10s-CVE-2026-119741`.
fn stream_major(branch: &str) -> Option<u32> {
    let rest = branch.strip_prefix('c')?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let after = rest[digits.len()..].strip_prefix('s')?;
    (after.is_empty() || after.starts_with('-'))
        .then(|| digits.parse().ok())
        .flatten()
}

/// The collections a branch builds into: its own, and the RHEL variant
/// Facebook's Stream branches also build for. A branch no SIG names,
/// such as Stream's `c9s` or a work branch `c10s-CVE-…`, is taken as
/// Proposed Updates' when the checkout's remotes point at that SIG's
/// repo and no other (`sigs`): its collection depends only on the
/// release. Hyperscale's also depends on the repository, which only
/// its branch names.
pub fn collections_for(branch: &str, sigs: &[Sig]) -> Result<Vec<Collection>, String> {
    let collection = match Sig::detect(branch) {
        Some((_, c)) => c,
        None => match (stream_major(branch), sigs) {
            (Some(major), [Sig::ProposedUpdates]) => {
                Collection::new(Sig::ProposedUpdates.name(), major, true, "main")
            }
            _ => {
                return Err(format!(
                    "{branch} is not a branch a known SIG builds from (c<N>s-proposed-<RHEL \
                     key> for Proposed Updates; c<N>s-hs, -hsx, -hs+fb, -hs+asahi or -hsk for \
                     Hyperscale), and the checkout's remotes do not settle which SIG it is \
                     for: pass --branch"
                ));
            }
        },
    };
    let rhel = collection.rhel_variant();
    Ok(std::iter::once(collection).chain(rhel).collect())
}

pub fn run(args: &AddPkgArgs) -> ExitCode {
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

/// The package the checkout's remotes are for.
fn package_of_checkout(repo: &Path) -> Result<String, String> {
    let remotes = git(repo, &["remote", "-v"])?;
    remotes
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .find_map(package_of_remote)
        .ok_or_else(|| {
            format!(
                "no remote of {} is a CentOS Stream or SIG repo: name the package",
                repo.display()
            )
        })
}

fn run_inner(args: &AddPkgArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let branch = match &args.branch {
        Some(b) => b.clone(),
        None => git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
    };
    let remote_urls = git(&repo, &["remote", "-v"]).unwrap_or_default();
    let sigs = sigs_of_remotes(
        remote_urls
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1)),
    );
    let collections = collections_for(&branch, &sigs)?;
    let package = match &args.package {
        Some(p) => p.clone(),
        None => package_of_checkout(&repo)?,
    };
    let mut tools = vec![("koji", "sudo dnf install koji", Some("--help"))];
    if !args.dry_run {
        tools.push(("cbs", "sudo dnf install centos-packager", Some("--help")));
    }
    sandogasa_cli::require_tools(&tools)?;

    let tags: Vec<String> = collections.iter().flat_map(stage_tags).collect();
    ui.step(&format!(
        "Check which of {}'s tags list {package} already",
        collections
            .iter()
            .map(Collection::prefix)
            .collect::<Vec<_>>()
            .join(" and ")
    ));
    let mut missing: Vec<&String> = Vec::new();
    for tag in &tags {
        // `cbs` is koji with CBS's profile, as the writes are shown.
        let argv: Vec<String> = ["cbs", "list-pkgs", "--tag", tag, "--package", &package]
            .map(String::from)
            .to_vec();
        ui.show_command(&argv);
        if !listed(tag, &package)? {
            missing.push(tag);
        }
    }
    if missing.is_empty() {
        println!("{package} is listed in every tag it builds through already.");
        return Ok(());
    }

    let owner = match &args.owner {
        Some(o) => o.clone(),
        None if args.dry_run => whoami().unwrap_or_else(|_| "<your CBS user>".to_string()),
        None => {
            sandogasa_cbs::login::ensure_login("cbspkg", true, args.explain)?;
            whoami()?
        }
    };
    if !args.dry_run {
        if !std::io::stdin().is_terminal() {
            return Err(
                "listing a package in CBS is confirmed at a terminal; run it in one".into(),
            );
        }
        let list = missing
            .iter()
            .map(|t| format!("  {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        println!("{package} is not listed in:\n{list}");
        if !sandogasa_cli::confirm(&format!("List it there, owned by {owner}?"), true)? {
            println!("Nothing changed.");
            return Ok(());
        }
    }
    for tag in missing {
        ui.step(&format!("List {package} in {tag}, owned by {owner}"));
        ui.run_required(&add_pkg_argv(&owner, tag, &package), &repo)?;
        if !args.dry_run {
            println!("Listed {package} in {tag}.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_is_read_from_a_stream_or_sig_remote() {
        for url in [
            "git+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git",
            "git@gitlab.com:CentOS/proposed_updates/rpms/rsync.git",
            "https://gitlab.com/CentOS/Hyperscale/rpms/rsync",
        ] {
            assert_eq!(package_of_remote(url).as_deref(), Some("rsync"), "{url}");
        }
        assert_eq!(
            package_of_remote("git+ssh://git@gitlab.com/michel-slm/centos_rpms_rsync.git"),
            None
        );
    }

    #[test]
    fn a_branch_names_its_collections() {
        let prefixes = |b: &str| {
            collections_for(b, &[])
                .unwrap()
                .iter()
                .map(Collection::prefix)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            prefixes("c10s-proposed-RHEL-303721"),
            ["proposed_updates10s-packages-main"]
        );
        assert_eq!(
            prefixes("c9s-hs+fb"),
            [
                "hyperscale9s-packages-facebook",
                "hyperscale9-packages-facebook"
            ]
        );
        let err = collections_for("c10s", &[]).unwrap_err();
        assert!(
            err.contains("c10s is not") && err.contains("--branch"),
            "{err}"
        );
    }

    /// rsync's checkout, on Stream's c9s, with the Proposed Updates
    /// repo as a remote: no SIG branch needed.
    #[test]
    fn a_stream_branch_is_proposed_updates_when_only_its_repo_is_a_remote() {
        let sigs = sigs_of_remotes([
            "git+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git",
            "git+ssh://git@gitlab.com/michel-slm/centos_rpms_rsync.git",
            "git+ssh://git@gitlab.com/CentOS/proposed_updates/rpms/rsync.git",
        ]);
        assert_eq!(sigs, [Sig::ProposedUpdates]);
        for branch in ["c9s", "c9s-CVE-2026-119741"] {
            let c = collections_for(branch, &sigs).unwrap();
            assert_eq!(
                c.iter().map(Collection::prefix).collect::<Vec<_>>(),
                ["proposed_updates9s-packages-main"],
                "{branch}"
            );
        }
        // Hyperscale's repository is not in a Stream branch's name.
        let hs = sigs_of_remotes(["git@gitlab.com:CentOS/Hyperscale/rpms/rsync.git"]);
        assert!(collections_for("c9s", &hs).is_err());
        // Nor is a SIG settled by no SIG remote, or by both.
        assert!(collections_for("c9s", &[]).is_err());
        assert!(collections_for("c9s", &[Sig::ProposedUpdates, Sig::Hyperscale]).is_err());
        assert!(collections_for("c9sx", &[Sig::ProposedUpdates]).is_err());
    }
}
