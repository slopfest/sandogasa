// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `upload-sources`: put the files a branch's `sources` names in the SIG
//! lookaside, where CBS fetches them (#99).
//!
//! A SIG build fetches its sources from the SIGs' lookaside cache, not
//! CentOS Stream's, so a package's first SIG build fails on a 404 for
//! every file (rsync's first, task 6077804) until each is uploaded with
//! `centos-lookaside-upload-sig`. The files usually come from Stream or
//! Fedora, so a file the checkout lacks is fetched from their
//! lookasides first, as `srpm` fetches it (`crate::fetch`), and every
//! one is checked against its sum before it is uploaded. Like
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

/// CentOS Stream's lookaside: `CBSPKG_STREAM_LOOKASIDE_BASE`, for
/// testing against a local server, else sources.stream.centos.org.
pub fn stream_lookaside_base() -> String {
    std::env::var("CBSPKG_STREAM_LOOKASIDE_BASE")
        .unwrap_or_else(|_| sandogasa_cbs::lookaside::STREAM_LOOKASIDE.to_string())
}

/// Fedora's lookaside: `CBSPKG_FEDORA_LOOKASIDE_BASE`, for testing
/// against a local server, else src.fedoraproject.org.
pub fn fedora_lookaside_base() -> String {
    std::env::var("CBSPKG_FEDORA_LOOKASIDE_BASE")
        .unwrap_or_else(|_| sandogasa_cbs::lookaside::FEDORA_LOOKASIDE.to_string())
}

/// What the SIG lookaside lacks, to follow "The SIG lookaside", and the
/// pronoun for it: one file by name ("lacks x.tar.gz, which CBS needs
/// to build", "it"), several by count ("lacks 2 files CBS needs to
/// build", "them"), for the caller to list one per line, so a long list
/// does not run on in one sentence.
pub fn lacking(names: &[&str]) -> (String, &'static str) {
    match names {
        [one] => (format!("lacks {one}, which CBS needs to build"), "it"),
        _ => (
            format!("lacks {} files CBS needs to build", names.len()),
            "them",
        ),
    }
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
pub(crate) fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
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
pub fn matches(repo: &Path, source: &Source) -> bool {
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

/// Which of `sources` the SIG lookaside lacks, asking it for each
/// file. Every file has to be listed with a sha512 sum: the SIG
/// lookaside takes no other.
pub fn lookaside_lacks(
    ui: &Ui,
    package: &str,
    sources: &[Source],
) -> Result<Vec<Source>, Box<dyn std::error::Error>> {
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
    let mut missing = Vec::new();
    for source in sources {
        let at = url(&base, package, source);
        ui.show_command(&["curl", "--head", "--fail", &at].map(String::from));
        if !in_lookaside(&http, &at)? {
            missing.push(source.clone());
        }
    }
    Ok(missing)
}

/// Upload `missing`, files in `repo` with the sums they are listed
/// with, to the SIG lookaside, after confirming at a terminal.
/// `Ok(false)` when the person declined.
pub fn upload(
    ui: &Ui,
    repo: &Path,
    package: &str,
    missing: &[Source],
) -> Result<bool, Box<dyn std::error::Error>> {
    if !ui.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, ui.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err(
                "uploading to the SIG lookaside is confirmed at a terminal; run it in one".into(),
            );
        }
        let names: Vec<&str> = missing.iter().map(|s| s.name.as_str()).collect();
        let (what, it) = lacking(&names);
        // Each command names its file, so several are not listed again.
        println!("The SIG lookaside {what}. To upload {it}:");
        for source in missing {
            sandogasa_cli::explain::print_command(&upload_argv(&source.name, package));
        }
        if !sandogasa_cli::confirm(&format!("Upload {it}?"), true)? {
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

/// Upload whichever of the files the `sources` file at `commit` names
/// the SIG lookaside lacks, fetching any the checkout lacks first, from
/// CentOS Stream's or Fedora's lookaside as `srpm` does, after
/// confirming at a terminal. `Ok(false)` when the person declined.
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
    let missing = lookaside_lacks(ui, package, &parse_sources(&text))?;
    if missing.is_empty() {
        println!("{package}'s sources are in the SIG lookaside already.");
        return Ok(true);
    }
    // No reminder to upload: that is the next step.
    let wanted: Vec<&Source> = missing.iter().collect();
    crate::fetch::fetch_missing(
        ui,
        repo,
        package,
        &wanted,
        &crate::fetch::Lookasides::from_env(),
        false,
    )?;
    upload(ui, repo, package, &missing)
}

fn run_inner(args: &UploadSourcesArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    sandogasa_cli::require_tools(&[("git", "sudo dnf install git-core", Some("--version"))])?;
    let checkout = resolve(&repo, args.branch.as_deref(), None, None)?;
    let located = crate::build::locate(&checkout, &repo)?;
    upload_missing(&ui, &repo, &checkout, &located.commit)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{EnvGuard, Fixture};

    const FILE: &str = "rsync-3.2.7.tar.gz";
    /// `printf 'data\n' | sha512sum`
    const SUM: &str = "73651d654c5ba73dd4b687f9dbbbdfc00884bf3dc1674e4cdcf762ff31778b29\
                       11e61e02b2a97c4055523eed2c4e6051b9902b0f91b4ca5b95e4c53cdf940b3d";
    const BRANCH: &str = "c10s-proposed-RHEL-1";

    fn sources() -> String {
        format!("SHA512 ({FILE}) = {SUM}\n")
    }

    fn dry_run(branch: Option<&str>, f: &Fixture) -> UploadSourcesArgs {
        UploadSourcesArgs {
            branch: branch.map(str::to_string),
            dry_run: true,
            explain: false,
            repo: Some(f.repo.clone()),
        }
    }

    /// A lookaside answering `status` for the file, and the env that
    /// points the tool at it.
    fn lookaside(status: usize) -> (mockito::ServerGuard, mockito::Mock, EnvGuard) {
        let mut server = mockito::Server::new();
        let mock = server
            .mock(
                "HEAD",
                format!("/sources/rsync/{FILE}/sha512/{SUM}/{FILE}").as_str(),
            )
            .with_status(status)
            .expect(1)
            .create();
        let env = EnvGuard::new(&[("CBSPKG_LOOKASIDE_BASE", &server.url())]);
        (server, mock, env)
    }

    #[test]
    #[serial_test::serial]
    fn a_branch_without_a_sources_file_has_nothing_to_upload() {
        let f = Fixture::proposed(BRANCH, &[]);
        let args = dry_run(None, &f);
        run_inner(&args).unwrap();
        assert_eq!(run(&args), ExitCode::SUCCESS);
    }

    #[test]
    #[serial_test::serial]
    fn sources_the_lookaside_has_are_not_uploaded() {
        let f = Fixture::proposed(BRANCH, &[("sources", &sources())]);
        let (_server, mock, _env) = lookaside(200);
        run_inner(&dry_run(None, &f)).unwrap();
        mock.assert();
    }

    #[test]
    #[serial_test::serial]
    fn a_missing_source_the_checkout_has_is_uploaded() {
        let f = Fixture::proposed(BRANCH, &[("sources", &sources())]);
        std::fs::write(f.repo.join(FILE), "data\n").unwrap();
        let (_server, mock, _env) = lookaside(404);
        // Nothing to fetch: the file is there, with its sum.
        run_inner(&dry_run(None, &f)).unwrap();
        mock.assert();
    }

    /// A file the checkout lacks is fetched as `srpm` fetches it, from
    /// the branch's `sources` even when another branch is checked out,
    /// before anything is uploaded; one there with another sum is the
    /// packager's, and an error.
    #[test]
    #[serial_test::serial]
    fn a_missing_source_the_checkout_lacks_is_fetched_first() {
        let f = Fixture::proposed(BRANCH, &[("sources", &sources())]);
        f.git(&["checkout", "--quiet", "-b", "other"]);
        std::fs::remove_file(f.repo.join("sources")).unwrap();
        f.git(&["commit", "--quiet", "-am", "elsewhere"]);
        let mut sig = mockito::Server::new();
        let mut stream = mockito::Server::new();
        let at = format!("/sources/rsync/{FILE}/sha512/{SUM}/{FILE}");
        sig.mock("HEAD", at.as_str()).with_status(404).create();
        sig.mock("GET", at.as_str()).with_status(404).create();
        let fetched = stream
            .mock(
                "GET",
                format!("/rpms/rsync/{FILE}/sha512/{SUM}/{FILE}").as_str(),
            )
            .with_body("data\n")
            .expect(1)
            .create();
        let _env = EnvGuard::new(&[
            ("CBSPKG_LOOKASIDE_BASE", &sig.url()),
            ("CBSPKG_STREAM_LOOKASIDE_BASE", &stream.url()),
        ]);
        // A dry run shows the fetches and fetches nothing.
        run_inner(&dry_run(Some(BRANCH), &f)).unwrap();
        assert!(!f.repo.join(FILE).exists());
        // A real one fetches, then stops at the CBS login the fake koji
        // refuses, before uploading.
        let real = UploadSourcesArgs {
            dry_run: false,
            ..dry_run(Some(BRANCH), &f)
        };
        assert!(run_inner(&real).is_err());
        fetched.assert();
        assert_eq!(
            std::fs::read_to_string(f.repo.join(FILE)).unwrap(),
            "data\n"
        );

        std::fs::write(f.repo.join(FILE), "other\n").unwrap();
        let err = run_inner(&real).unwrap_err().to_string();
        assert!(err.contains("in the checkout does not match"), "{err}");
    }

    #[test]
    #[serial_test::serial]
    fn a_source_listed_with_another_hash_is_refused() {
        let f = Fixture::proposed(BRANCH, &[("sources", "MD5 (a.tar.gz) = 0123\n")]);
        let err = run_inner(&dry_run(None, &f)).unwrap_err().to_string();
        assert!(err.contains("takes sha512 sums"), "{err}");
        assert_eq!(run(&dry_run(None, &f)), ExitCode::FAILURE);
    }

    #[test]
    #[serial_test::serial]
    fn a_lookaside_error_is_not_taken_for_a_missing_file() {
        let f = Fixture::proposed(BRANCH, &[("sources", &sources())]);
        let (_server, _mock, _env) = lookaside(500);
        let err = run_inner(&dry_run(None, &f)).unwrap_err().to_string();
        assert!(err.contains("answered HTTP 500"), "{err}");
    }

    #[test]
    #[serial_test::serial]
    fn what_the_lookaside_lacks_is_named_as_one_file_or_several() {
        assert_eq!(
            lacking(&["a.tar.gz"]),
            ("lacks a.tar.gz, which CBS needs to build".to_string(), "it")
        );
        assert_eq!(
            lacking(&["a.tar.gz", "b.tar.gz"]),
            ("lacks 2 files CBS needs to build".to_string(), "them")
        );
    }

    #[test]
    #[serial_test::serial]
    fn the_lookaside_can_be_pointed_elsewhere() {
        let _env = EnvGuard::new(&[]).unset(&["CBSPKG_LOOKASIDE_BASE"]);
        assert_eq!(lookaside_base(), SIG_LOOKASIDE);
        let _env = EnvGuard::new(&[("CBSPKG_LOOKASIDE_BASE", "http://127.0.0.1:1")]);
        assert_eq!(lookaside_base(), "http://127.0.0.1:1");
        let _env = EnvGuard::new(&[]).unset(&["CBSPKG_STREAM_LOOKASIDE_BASE"]);
        assert_eq!(
            stream_lookaside_base(),
            sandogasa_cbs::lookaside::STREAM_LOOKASIDE
        );
        let _env = EnvGuard::new(&[("CBSPKG_STREAM_LOOKASIDE_BASE", "http://127.0.0.1:2")]);
        assert_eq!(stream_lookaside_base(), "http://127.0.0.1:2");
        let _env = EnvGuard::new(&[]).unset(&["CBSPKG_FEDORA_LOOKASIDE_BASE"]);
        assert_eq!(
            fedora_lookaside_base(),
            sandogasa_cbs::lookaside::FEDORA_LOOKASIDE
        );
        let _env = EnvGuard::new(&[("CBSPKG_FEDORA_LOOKASIDE_BASE", "http://127.0.0.1:3")]);
        assert_eq!(fedora_lookaside_base(), "http://127.0.0.1:3");
    }

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
        // Not left to whichever test happened to run first.
        sandogasa_cli::install_crypto_provider();
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
