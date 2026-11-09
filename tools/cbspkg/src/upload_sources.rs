// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `upload-sources`: put the files a branch's `sources` names in the SIG
//! lookaside, where CBS fetches them (#99).
//!
//! A SIG build fetches its sources from the SIGs' lookaside cache, not
//! CentOS Stream's, so a package's first SIG build fails on a 404 for
//! every file (rsync's first, task 6077804) until each is uploaded with
//! `centos-lookaside-upload-sig`. The files usually come from Stream,
//! so a file the checkout lacks is fetched with `centpkg sources` first,
//! and every one is checked against its sum before it is uploaded. Like
//! the CBS writes, the upload is confirmed at a terminal. `build` runs
//! the same check before building.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use sandogasa_cbs::lookaside::{SIG_LOOKASIDE, Source, parse_sources, upload_argv, url};
use sandogasa_cli::explain::Ui;

use crate::add_pkg::{Checkout, resolve};

#[derive(clap::Args)]
pub struct UploadSourcesArgs {
    /// The branch whose sources to upload (default: the one checked
    /// out)
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long)]
    pub explain: bool,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,
}

/// The checkouts around `centpkg sources`: to the branch, and back.
#[derive(Debug, PartialEq)]
pub struct Switch {
    pub there: Vec<String>,
    pub back: Vec<String>,
}

/// The SIG lookaside: `CBSPKG_LOOKASIDE_BASE`, for testing against a
/// local server, else git.centos.org.
pub fn lookaside_base() -> String {
    std::env::var("CBSPKG_LOOKASIDE_BASE").unwrap_or_else(|_| SIG_LOOKASIDE.to_string())
}

/// The sha512 `sha512sum` prints for a file.
pub fn parse_sha512sum(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .next()
        .filter(|s| s.len() == 128 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

/// Whether `centos-lookaside-upload-sig` succeeded: 0 is uploaded, 3 is
/// there already (it checks before uploading).
pub fn upload_succeeded(code: i32) -> bool {
    matches!(code, 0 | 3)
}

pub fn run(args: &UploadSourcesArgs) -> ExitCode {
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

/// Whether the lookaside has the file at `url`: a HEAD that answers 200.
fn in_lookaside(http: &reqwest::blocking::Client, url: &str) -> Result<bool, String> {
    let resp = http
        .head(url)
        .send()
        .map_err(|e| format!("cannot reach the SIG lookaside: {e}"))?;
    match resp.status().as_u16() {
        200 => Ok(true),
        404 => Ok(false),
        other => Err(format!("the SIG lookaside answered HTTP {other} for {url}")),
    }
}

/// Whether `file` in `repo` has the sum `source` names.
fn matches(repo: &Path, source: &Source) -> bool {
    if source.hash != "sha512" || !repo.join(&source.name).is_file() {
        return false;
    }
    Command::new("sha512sum")
        .arg(&source.name)
        .current_dir(repo)
        .output()
        .ok()
        .and_then(|o| parse_sha512sum(&String::from_utf8_lossy(&o.stdout)))
        .is_some_and(|sum| sum.eq_ignore_ascii_case(&source.sum))
}

/// How to get `branch` (at `head`'s `commit`) checked out for
/// `centpkg sources`, which fetches the checked-out branch's files, and
/// back, or `None` when HEAD is at `commit` already.
/// `current` is `git rev-parse --abbrev-ref HEAD` (`HEAD` when
/// detached); `has_local` whether `branch` exists locally, else it is
/// checked out tracking `remote`'s. A checkout with uncommitted changes
/// (`dirty`) is not switched: that is an error saying how to proceed.
/// The same as cpu-sig-tracker's `mr` does for the pre-push hook.
pub fn switch_for_sources(
    current: &str,
    head: &str,
    commit: &str,
    branch: &str,
    has_local: bool,
    remote: &str,
    dirty: bool,
) -> Result<Option<Switch>, String> {
    if head == commit {
        return Ok(None);
    }
    if dirty {
        return Err(format!(
            "fetching {branch}'s sources means checking it out (`centpkg sources` fetches the \
             checked-out branch's), and the checkout has uncommitted changes: commit or stash \
             them, or run `git checkout {branch}` yourself"
        ));
    }
    let argv = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let there = if has_local {
        argv(&["git", "checkout", "--quiet", branch])
    } else {
        argv(&[
            "git",
            "checkout",
            "--quiet",
            "--track",
            &format!("{remote}/{branch}"),
        ])
    };
    let back_to = if current == "HEAD" { head } else { current };
    Ok(Some(Switch {
        there,
        back: argv(&["git", "checkout", "--quiet", back_to]),
    }))
}

/// Upload whichever of the files the `sources` file at `commit` names
/// the SIG lookaside lacks, fetching any the checkout lacks with
/// `centpkg sources` first (on the branch, switching to it and back when
/// it is not checked out), after confirming at a terminal. `Ok(false)`
/// when the person declined.
pub fn upload_missing(
    ui: &Ui,
    repo: &Path,
    checkout: &Checkout,
    remote: &str,
    commit: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let package = &checkout.package;
    let Ok(text) = git(repo, &["show", &format!("{commit}:sources")]) else {
        // No sources file: nothing to fetch.
        return Ok(true);
    };
    let sources = parse_sources(&text);
    if let Some(other) = sources.iter().find(|s| s.hash != "sha512") {
        return Err(format!(
            "{} is listed with {}; the SIG lookaside takes sha512 sums",
            other.name, other.hash
        )
        .into());
    }
    let base = lookaside_base();
    let http = sandogasa_cli::http::blocking_builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .build()?;
    ui.step(&format!(
        "Check which of {package}'s sources the SIG lookaside has"
    ));
    let mut missing: Vec<&Source> = Vec::new();
    for source in &sources {
        let at = url(&base, package, source);
        ui.show_command(&["curl", "--head", "--fail", &at].map(String::from));
        if !in_lookaside(&http, &at)? {
            missing.push(source);
        }
    }
    if missing.is_empty() {
        println!("{package}'s sources are in the SIG lookaside already.");
        return Ok(true);
    }

    // The files themselves, from CentOS Stream's lookaside when the
    // checkout lacks them. `centpkg sources` fetches what the checked-out
    // branch's `sources` names, so the branch is checked out for it and
    // the checkout put back after; the files it fetches are untracked,
    // so they stay for the upload.
    if missing.iter().any(|s| !matches(repo, s)) {
        sandogasa_cli::require_tools(&[("centpkg", "sudo dnf install centpkg", Some("--help"))])?;
        let branch = &checkout.branch;
        let switch = switch_for_sources(
            &git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?,
            &git(repo, &["rev-parse", "HEAD"])?,
            commit,
            branch,
            git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .is_ok(),
            remote,
            !git(repo, &["status", "--porcelain", "--untracked-files=no"])?.is_empty(),
        )?;
        if let Some(Switch { there, .. }) = &switch {
            ui.step(&format!(
                "Check out {branch}: `centpkg sources` fetches the checked-out branch's files"
            ));
            ui.run_required(there, repo)?;
        }
        ui.step("Fetch them from CentOS Stream's lookaside");
        let fetched = ui.run_required(&["centpkg", "sources"].map(String::from), repo);
        if let Some(Switch { back, .. }) = &switch {
            ui.step("Switch the checkout back");
            ui.run_required(back, repo)?;
        }
        fetched?;
        if !ui.dry_run
            && let Some(bad) = missing.iter().find(|s| !matches(repo, s))
        {
            return Err(format!(
                "{} does not have the sum `sources` names after `centpkg sources`; it may \
                 not be in CentOS Stream's lookaside: upload it by hand",
                bad.name
            )
            .into());
        }
    }

    if !ui.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, ui.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err(
                "uploading to the SIG lookaside is confirmed at a terminal; run it in one".into(),
            );
        }
        println!("The SIG lookaside lacks:");
        for source in &missing {
            println!(
                "  {}",
                sandogasa_cli::explain::command_line(&upload_argv(&source.name, package))
            );
        }
        if !sandogasa_cli::confirm("Upload them?", true)? {
            println!("Nothing uploaded.");
            return Ok(false);
        }
    }
    // Its -h exits 1, so only that it is on PATH is checked.
    sandogasa_cli::require_tools(&[(
        "centos-lookaside-upload-sig",
        "sudo dnf install centos-packager",
        None,
    )])?;
    for source in missing {
        ui.step(&format!("Upload {} to the SIG lookaside", source.name));
        let code = ui.run_status(&upload_argv(&source.name, package), repo)?;
        if !upload_succeeded(code) {
            return Err(format!(
                "centos-lookaside-upload-sig failed for {} (exit {code})",
                source.name
            )
            .into());
        }
    }
    Ok(true)
}

fn run_inner(args: &UploadSourcesArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    sandogasa_cli::require_tools(&[("git", "sudo dnf install git-core", Some("--version"))])?;
    let checkout = resolve(&repo, args.branch.as_deref(), None)?;
    let located = crate::build::locate(&checkout, &repo)?;
    upload_missing(&ui, &repo, &checkout, &located.remote, &located.commit)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sha512sum_line_gives_the_sum() {
        let sum = "c2afba11a352fd88133f9e96e19d6df80eb864450c83eced13a7faa23df947bccf2ef093f2101df6\
                   ee30abff4cbbd39ac802e9aa5f726e42c9caff274fad8377";
        assert_eq!(
            parse_sha512sum(&format!("{sum}  rsync-3.2.7.tar.gz\n")).as_deref(),
            Some(sum)
        );
        assert_eq!(parse_sha512sum("sha512sum: x: No such file\n"), None);
        assert_eq!(parse_sha512sum(""), None);
    }

    #[test]
    fn an_upload_of_a_file_already_there_is_not_a_failure() {
        assert!(upload_succeeded(0));
        assert!(upload_succeeded(3));
        assert!(!upload_succeeded(1));
        assert!(!upload_succeeded(2));
    }

    #[test]
    fn the_lookaside_is_asked_with_a_head_request() {
        let mut server = mockito::Server::new();
        server
            .mock("HEAD", "/sources/rsync/a.tar.gz/sha512/1/a.tar.gz")
            .with_status(200)
            .create();
        server
            .mock("HEAD", "/sources/rsync/b.tar.gz/sha512/2/b.tar.gz")
            .with_status(404)
            .create();
        let http = reqwest::blocking::Client::new();
        let sources = parse_sources("SHA512 (a.tar.gz) = 1\nSHA512 (b.tar.gz) = 2\n");
        let has = |s: &Source| in_lookaside(&http, &url(&server.url(), "rsync", s)).unwrap();
        assert!(has(&sources[0]));
        assert!(!has(&sources[1]));
    }

    #[test]
    fn the_branch_is_checked_out_for_centpkg_sources_and_the_checkout_put_back() {
        let line = |a: &Vec<String>| a.join(" ");
        // rsync's c10s build from a checkout on the c9s branch.
        let Switch { there, back } = switch_for_sources(
            "c9s-proposed-RHEL-303761",
            "3afcfa97",
            "e4441b13",
            "c10s-CVE-2026-119741",
            true,
            "sig",
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(line(&there), "git checkout --quiet c10s-CVE-2026-119741");
        assert_eq!(line(&back), "git checkout --quiet c9s-proposed-RHEL-303761");
        // Only on the SIG's remote: checked out tracking it.
        let Switch { there, .. } = switch_for_sources(
            "c9s",
            "3afcfa97",
            "e4441b13",
            "c10s-proposed-RHEL-303721",
            false,
            "sig",
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            line(&there),
            "git checkout --quiet --track sig/c10s-proposed-RHEL-303721"
        );
        // A detached HEAD goes back to its commit.
        let Switch { back, .. } =
            switch_for_sources("HEAD", "3afcfa97", "e4441b13", "b", true, "sig", false)
                .unwrap()
                .unwrap();
        assert_eq!(line(&back), "git checkout --quiet 3afcfa97");
        // At the commit already: nothing to switch.
        assert_eq!(
            switch_for_sources("x", "e4441b13", "e4441b13", "b", true, "sig", true),
            Ok(None)
        );
        let err =
            switch_for_sources("x", "3afcfa97", "e4441b13", "b", true, "sig", true).unwrap_err();
        assert!(
            err.contains("uncommitted changes") && err.contains("git checkout b"),
            "{err}"
        );
    }
}
