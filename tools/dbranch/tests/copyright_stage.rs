// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The copyright stage of `dbranch update`, end to end, against stub
//! `cme`, `lrc`, `decopy` and `dpkg-source` scripts.
//!
//! Each run gets a `PATH` holding only the stubs and git, so what the
//! host has installed cannot change the outcome, and the binary runs
//! as a child process, so the stubs never reach other tests through
//! the environment. stdin is not a terminal, so nothing is offered.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A packaging repository on `debian/unstable` whose
/// `debian/copyright` says `copyright` (none when `None`), at
/// `fnlfmt/` inside the returned directory: the stage writes its scan
/// beside the repository, which has to be somewhere the test owns.
fn repo(copyright: Option<&str>, patches: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = &dir.path().join("fnlfmt");
    std::fs::create_dir(p).unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(p)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q", "-b", "debian/unstable"]);
    git(&["config", "user.name", "T"]);
    git(&["config", "user.email", "t@x"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(p.join("debian")).unwrap();
    std::fs::write(
        p.join("debian/changelog"),
        "fnlfmt (0.3.2-1) unstable; urgency=medium\n\n  * x\n\n \
         -- M <m@x>  Wed, 17 Jun 2026 17:28:43 +0100\n",
    )
    .unwrap();
    if let Some(text) = copyright {
        std::fs::write(p.join("debian/copyright"), text).unwrap();
    }
    if patches {
        std::fs::create_dir_all(p.join("debian/patches")).unwrap();
        std::fs::write(p.join("debian/patches/series"), "").unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
    dir
}

/// A directory of stub tools, `(name, shell body)`, plus git.
fn stubs(tools: &[(&str, &str)]) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in tools {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink(which("git"), dir.path().join("git")).unwrap();
    dir
}

fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not found"))
}

/// Run `dbranch update --stage copyright` and return its stderr.
fn copyright_stage(repo: &Path, bin: &Path) -> String {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dbranch"))
        .args(["--no-defaults", "update", "--stage", "copyright"])
        .current_dir(repo)
        .env("PATH", bin)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    stderr
}

const COPYRIGHT: &str = "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n\n\
                         Files: *\nCopyright: 2019-2024 Phil Hagelberg\nLicense: Expat\n";

#[test]
fn a_license_mismatch_and_a_rejected_file_are_reported_with_their_sources() {
    let repo = repo(Some(COPYRIGHT), false);
    let bin = stubs(&[
        ("cme", "echo \"Warning in 'Files:*': odd\""),
        ("lrc", "echo 'x.c: GPL-2+ != Expat'; exit 3"),
    ]);
    let err = copyright_stage(&repo.path().join("fnlfmt"), bin.path());
    assert!(err.contains("Warning in 'Files:*': odd"), "{err}");
    assert!(err.contains("warning: cme is unhappy"), "{err}");
    assert!(
        err.contains("see: DEP-5, Machine-readable debian/copyright file"),
        "{err}"
    );
    assert!(err.contains("x.c: GPL-2+ != Expat"), "{err}");
    assert!(
        err.contains("warning: debian/copyright disagrees with the source"),
        "{err}"
    );
    assert!(
        err.contains("https://wiki.debian.org/CopyrightReviewTools#licenserecon"),
        "{err}"
    );
}

#[test]
fn a_passing_file_is_still_checked_for_placeholders_holders_and_years() {
    let repo = repo(
        Some(&COPYRIGHT.replace("License: Expat", "License: Expat\nComment: TODO")),
        true,
    );
    // decopy writes its scan to the path after -o.
    let bin = stubs(&[
        ("cme", ""),
        ("lrc", "exit 0"),
        ("dpkg-source", "exit 0"),
        (
            "decopy",
            "while [ \"$1\" != -o ]; do shift; done\n\
             printf 'Files: *\\nCopyright: 2019-2026, Phil Hagelberg\\n           2020, Calvin Rose\\n' > \"$2\"",
        ),
    ]);
    let err = copyright_stage(&repo.path().join("fnlfmt"), bin.path());
    assert!(err.contains("still says TODO"), "{err}");
    assert!(err.contains("claims 2024 at the latest"), "{err}");
    assert!(err.contains("names Calvin Rose"), "{err}");
    assert!(err.contains("../fnlfmt.copyright.decopy"), "{err}");
    assert!(repo.path().join("fnlfmt.copyright.decopy").is_file());
    assert!(
        err.contains("https://wiki.debian.org/CopyrightReviewTools#decopy"),
        "{err}"
    );
    assert!(!err.contains("cme is unhappy"), "silence is a pass: {err}");
}

#[test]
fn a_failed_scan_is_a_note() {
    let repo = repo(Some(COPYRIGHT), false);
    let bin = stubs(&[("lrc", "exit 0"), ("decopy", "exit 1")]);
    let err = copyright_stage(&repo.path().join("fnlfmt"), bin.path());
    assert!(
        err.contains("decopy failed; copyright holders not compared"),
        "{err}"
    );
}

#[test]
fn without_lrc_or_a_copyright_file_the_stage_only_notes_it() {
    let bin = stubs(&[]);
    let repo_without_file = repo(None, false);
    let err = copyright_stage(&repo_without_file.path().join("fnlfmt"), bin.path());
    assert!(err.contains("note: no debian/copyright to check"), "{err}");

    let repo_with_file = repo(Some(COPYRIGHT), false);
    let err = copyright_stage(&repo_with_file.path().join("fnlfmt"), bin.path());
    assert!(err.contains("install licenserecon for lrc"), "{err}");
}
