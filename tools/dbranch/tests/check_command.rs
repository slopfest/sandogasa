// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `dbranch check` as a whole, in a fixture packaging repository, with
//! a stub `patch`. As in the other tests here, each run gets a `PATH`
//! holding only the stubs and git, and the binary runs as a child.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?}"
    );
}

/// A 3.0 (quilt) package with one patch and a gbp.conf naming an
/// upstream branch that does not exist.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = &dir.path().join("damo");
    std::fs::create_dir_all(p.join("debian/source")).unwrap();
    std::fs::create_dir_all(p.join("debian/patches")).unwrap();
    git(p, &["init", "-q", "-b", "debian/unstable"]);
    git(p, &["config", "user.name", "T"]);
    git(p, &["config", "user.email", "t@x"]);
    git(p, &["config", "commit.gpgsign", "false"]);
    let write = |path: &str, text: &str| std::fs::write(p.join(path), text).unwrap();
    write(
        "debian/changelog",
        "damo (3.2.8-1) unstable; urgency=medium\n\n  * x\n\n \
         -- M <m@x>  Wed, 17 Jun 2026 17:28:43 +0100\n",
    );
    write("debian/source/format", "3.0 (quilt)\n");
    write(
        "debian/control",
        "Source: damo\nMaintainer: M <m@x>\n\nPackage: damo\nArchitecture: all\n",
    );
    write(
        "debian/gbp.conf",
        "[DEFAULT]\ndebian-branch = debian/unstable\nupstream-branch = upstream/latest\n",
    );
    write(
        "debian/patches/fix.patch",
        "From: A <a@x>\nSubject: fix\n\n--- a/damo.py\n+++ b/damo.py\n",
    );
    write("debian/patches/series", "fix.patch\n");
    write("damo.py", "print('damo')\n");
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "packaging"]);
    dir
}

fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not found"))
}

/// Stubs: `patch` exits `patch_exit`; git is the real one.
fn stubs(patch_exit: i32) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("patch");
    std::fs::write(&path, format!("#!/bin/sh\nexit {patch_exit}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(which("git"), dir.path().join("git")).unwrap();
    dir
}

/// Run `dbranch check` with `args`; whether it passed, and its
/// stdout and stderr.
fn check(repo: &Path, bin: &Path, args: &[&str]) -> (bool, String, String) {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dbranch"))
        .args(["--no-defaults", "check"])
        .args(args)
        .current_dir(repo)
        .env("PATH", bin)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_patch_that_no_longer_applies_is_a_blocker_with_its_sources() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(1);
    let (ok, out, err) = check(p, bin.path(), &[]);
    assert!(!ok, "a blocker fails the run: {out}{err}");
    assert!(
        out.contains("blocker: debian/patches/fix.patch no longer applies"),
        "{out}"
    );
    assert!(
        out.contains("see: dpkg-source(1), under Format: 3.0 (quilt)"),
        "{out}"
    );
    assert!(
        out.contains("upstream/latest"),
        "gbp.conf's missing branch: {out}"
    );
}

#[test]
fn the_json_report_carries_each_findings_sources() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(0);
    let (_, out, err) = check(p, bin.path(), &["--json"]);
    let findings: serde_json::Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}{err}"));
    let findings = findings.as_array().unwrap();
    assert!(
        findings
            .iter()
            .all(|f| f["check"] != "patch-does-not-apply"),
        "the patch applies: {out}"
    );
    let missing = findings
        .iter()
        .find(|f| f["check"] == "gbp-branch-missing")
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(missing["sources"][0]["cite"], "gbp.conf(5)");
}

#[test]
fn a_clean_tree_with_nothing_to_report_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    git(p, &["init", "-q", "-b", "debian/unstable"]);
    git(p, &["config", "user.name", "T"]);
    git(p, &["config", "user.email", "t@x"]);
    git(p, &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(p.join("debian/source")).unwrap();
    std::fs::write(p.join("debian/source/format"), "3.0 (native)\n").unwrap();
    std::fs::write(
        p.join("debian/changelog"),
        "damo (3.2.8) unstable; urgency=medium\n\n  * x\n\n \
         -- M <m@x>  Wed, 17 Jun 2026 17:28:43 +0100\n",
    )
    .unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "init"]);
    let bin = stubs(0);
    let (ok, out, err) = check(p, bin.path(), &[]);
    assert!(ok, "{out}{err}");
}
