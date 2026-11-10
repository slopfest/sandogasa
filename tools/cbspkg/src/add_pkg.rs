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
use sandogasa_cbs::remotes::Remotes;
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

/// What a checkout is about: its branch, the collections that branch
/// builds into, the package, and its remotes `(name, URL)`.
pub struct Checkout {
    pub branch: String,
    pub collections: Vec<Collection>,
    pub package: String,
    pub remotes: Remotes,
    /// The SIG whose repo the checkout builds from.
    pub sig: Sig,
}

impl Checkout {
    /// What a person should see before a build or promotion: which
    /// pipeline the package goes through, when it is not the SIG's own.
    pub fn pipeline_note(&self) -> Option<String> {
        let c = self.collections.iter().find(|c| c.is_extras())?;
        Some(format!(
            "{} is the {} SIG's release package: it is built in Extras, not the SIG's \
             collection, with target {} and tags {}-{{candidate,testing,release}}.",
            self.package,
            self.sig.name(),
            c.target(),
            c.prefix()
        ))
    }
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
        None => match (sandogasa_cbs::stream_major(branch), sigs) {
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

/// Work out [`Checkout`] for `repo`, with `branch` and `package`
/// overriding what the checkout says.
pub fn resolve(
    repo: &Path,
    branch: Option<&str>,
    package: Option<&str>,
) -> Result<Checkout, Box<dyn std::error::Error>> {
    let branch = match branch {
        Some(b) => b.to_string(),
        None => git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
    };
    let remotes = Remotes::parse(&git(repo, &["remote", "-v"]).unwrap_or_default());
    let package = package
        .map(str::to_string)
        .or_else(|| remotes.package().map(str::to_string));
    // The branch's collections first: an unknown branch is the error to
    // report, whatever the package.
    let (sig, collections) =
        collections_of(package.as_deref().unwrap_or(""), &branch, &remotes.sigs())?;
    let package = package.ok_or_else(|| {
        format!(
            "no remote of {} is a CentOS Stream or SIG repo: name the package",
            repo.display()
        )
    })?;
    let checkout = Checkout {
        branch,
        collections,
        package,
        remotes,
        sig,
    };
    if let Some(note) = checkout.pipeline_note() {
        println!("{note}");
    }
    Ok(checkout)
}

/// The SIG and collections `package` on `branch` builds through. A
/// SIG's release package (`centos-release-*`) is built from the SIG's
/// repo into Extras' common collection, so its SIG is the one it is
/// named after (`centos-release-hyperscale`), else the one whose repo
/// the checkout's remotes point at (`sigs`), when only one; any other
/// package's SIG is its collection's.
pub fn collections_of(
    package: &str,
    branch: &str,
    sigs: &[Sig],
) -> Result<(Sig, Vec<Collection>), String> {
    if let Some(c) = sandogasa_cbs::release_package_collection(package, branch) {
        let named = Sig::ALL
            .into_iter()
            .find(|s| s.release_package() == package);
        return match (named, sigs) {
            (Some(sig), _) | (None, &[sig]) => Ok((sig, vec![c])),
            _ => Err(format!(
                "{package} is a SIG's release package, built from the SIG's repo into {}, and \
                 the checkout's remotes do not settle which SIG's repo that is: they should \
                 point at exactly one",
                c.target()
            )),
        };
    }
    let collections = collections_for(branch, sigs)?;
    let sig = Sig::ALL
        .into_iter()
        .find(|s| s.name() == collections[0].sig)
        .ok_or("the branch's SIG has no known GitLab group")?;
    Ok((sig, collections))
}

/// List `package` in whichever of the collections' tags do not list it
/// yet, owned by `owner` (default: the CBS login), after confirming at
/// a terminal. `Ok(false)` when the person declined.
pub fn list_missing(
    ui: &Ui,
    repo: &Path,
    collections: &[Collection],
    package: &str,
    owner: Option<&str>,
) -> Result<bool, Box<dyn std::error::Error>> {
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
        let argv: Vec<String> = ["cbs", "list-pkgs", "--tag", tag, "--package", package]
            .map(String::from)
            .to_vec();
        ui.show_command(&argv);
        if !listed(tag, package)? {
            missing.push(tag);
        }
    }
    if missing.is_empty() {
        println!("{package} is listed in every tag it builds through already.");
        return Ok(true);
    }

    let owner = match owner {
        Some(o) => o.to_string(),
        None if ui.dry_run => whoami().unwrap_or_else(|_| "<your CBS user>".to_string()),
        None => {
            sandogasa_cbs::login::ensure_login("cbspkg", true, ui.explain)?;
            whoami()?
        }
    };
    if !ui.dry_run {
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
            return Ok(false);
        }
    }
    for tag in missing {
        ui.step(&format!("List {package} in {tag}, owned by {owner}"));
        ui.run_required(&add_pkg_argv(&owner, tag, package), repo)?;
        if !ui.dry_run {
            println!("Listed {package} in {tag}.");
        }
    }
    Ok(true)
}

fn run_inner(args: &AddPkgArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let checkout = resolve(&repo, args.branch.as_deref(), args.package.as_deref())?;
    let mut tools = vec![("koji", "sudo dnf install koji", Some("--help"))];
    if !args.dry_run {
        tools.push(("cbs", "sudo dnf install centos-packager", Some("--help")));
    }
    sandogasa_cli::require_tools(&tools)?;
    list_missing(
        &ui,
        &repo,
        &checkout.collections,
        &checkout.package,
        args.owner.as_deref(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let sigs = Remotes::parse(
            "origin\tgit+ssh://git@gitlab.com/redhat/centos-stream/rpms/rsync.git (fetch)\n\
             michel-slm\tgit+ssh://git@gitlab.com/michel-slm/centos_rpms_rsync.git (fetch)\n\
             sig\tgit+ssh://git@gitlab.com/CentOS/proposed_updates/rpms/rsync.git (fetch)\n",
        )
        .sigs();
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
        let hs =
            Remotes::parse("origin\tgit@gitlab.com:CentOS/Hyperscale/rpms/rsync.git (fetch)\n")
                .sigs();
        assert!(collections_for("c9s", &hs).is_err());
        // Nor is a SIG settled by no SIG remote, or by both.
        assert!(collections_for("c9s", &[]).is_err());
        assert!(collections_for("c9s", &[Sig::ProposedUpdates, Sig::Hyperscale]).is_err());
        assert!(collections_for("c9sx", &[Sig::ProposedUpdates]).is_err());
    }

    /// A SIG's release package goes through Extras on the branches it is
    /// built from, not the SIG's collection the branch would otherwise
    /// name (Hyperscale's c10s-sig-hyperscale is its main repository).
    #[test]
    fn a_release_package_builds_through_extras_from_its_sigs_repo() {
        let (sig, c) = collections_of(
            "centos-release-hyperscale",
            "c10s-sig-hyperscale",
            &[Sig::Hyperscale],
        )
        .unwrap();
        assert_eq!(sig, Sig::Hyperscale);
        assert_eq!(
            c.iter().map(Collection::target).collect::<Vec<_>>(),
            ["extras10s-extras-common-el10s"]
        );
        let (sig, c) = collections_of(
            "centos-release-proposed_updates",
            "c9s-proposed",
            &[Sig::ProposedUpdates],
        )
        .unwrap();
        assert_eq!(sig, Sig::ProposedUpdates);
        assert_eq!(c[0].prefix(), "extras9s-extras-common");
        let checkout = Checkout {
            branch: "c9s-proposed".into(),
            collections: c,
            package: "centos-release-proposed_updates".into(),
            remotes: Remotes::default(),
            sig,
        };
        let note = checkout.pipeline_note().unwrap();
        assert!(
            note.contains("built in Extras") && note.contains("extras9s-extras-common-el9s"),
            "{note}"
        );
        // The SIG is the package's, whatever else the remotes name.
        let (sig, _) = collections_of(
            "centos-release-hyperscale",
            "c10s-sig-hyperscale",
            &[Sig::ProposedUpdates, Sig::Hyperscale],
        )
        .unwrap();
        assert_eq!(sig, Sig::Hyperscale);
        // A release package not named after the SIG goes to Extras too,
        // from the one SIG repo the remotes name.
        let (sig, c) = collections_of(
            "centos-release-hyperscale-kernel",
            "c9s-hs+fb",
            &[Sig::Hyperscale],
        )
        .unwrap();
        assert_eq!(sig, Sig::Hyperscale);
        assert_eq!(
            c.iter().map(Collection::prefix).collect::<Vec<_>>(),
            ["extras9s-extras-common"]
        );
        let err = collections_of("centos-release-ceph-reef", "c10s", &[]).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        // An unknown branch is reported as such, release package or not.
        let err = collections_of("", "c10s", &[]).unwrap_err();
        assert!(err.contains("c10s is not"), "{err}");
        // Any other package keeps its branch's collection, and no note.
        let (sig, c) = collections_of("rsync", "c9s-proposed-RHEL-1", &[]).unwrap();
        assert_eq!(
            (sig, c[0].prefix().as_str()),
            (Sig::ProposedUpdates, "proposed_updates9s-packages-main")
        );
    }
}
