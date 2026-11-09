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

/// Upload whichever of the files the `sources` file at `commit` names
/// the SIG lookaside lacks, fetching any the checkout lacks with
/// `centpkg sources` first, after confirming at a terminal. `Ok(false)`
/// when the person declined.
pub fn upload_missing(
    ui: &Ui,
    repo: &Path,
    checkout: &Checkout,
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
    // branch's `sources` names, so that has to be this commit's.
    if missing.iter().any(|s| !matches(repo, s)) {
        let head = git(repo, &["rev-parse", "HEAD"])?;
        if head != commit {
            return Err(format!(
                "the checkout lacks some of {}'s sources, and `centpkg sources` fetches the \
                 checked-out branch's: check out {} first",
                checkout.branch, checkout.branch
            )
            .into());
        }
        sandogasa_cli::require_tools(&[("centpkg", "sudo dnf install centpkg", Some("--help"))])?;
        ui.step("Fetch them from CentOS Stream's lookaside");
        ui.run_required(&["centpkg", "sources"].map(String::from), repo)?;
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
    upload_missing(&ui, &repo, &checkout, &located.commit)?;
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
}
