// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `dbranch rebuild` and `update` as a whole, under `--dry-run`, with
//! stub `ubuntu-distro-info` and `debian-distro-info` so the branch
//! selection does not depend on the host's distro-info data.
//!
//! As in `copyright_stage.rs`, each run gets a `PATH` holding only the
//! stubs and git, and the binary runs as a child process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A packaging repository on `debian/unstable`, with an `origin`, PPA
/// branches for three Ubuntu releases (focal end-of-life), and the
/// Debian branches `debian/trixie` (a proposed-update) and
/// `debian/trixie-backports`.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = &dir.path().join("damo");
    std::fs::create_dir_all(p.join("debian")).unwrap();
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
    git(&[
        "remote",
        "add",
        "origin",
        "https://salsa.debian.org/x/damo.git",
    ]);
    std::fs::write(
        p.join("debian/changelog"),
        "damo (3.2.8-1) unstable; urgency=medium\n\n  * x\n\n \
         -- M <m@x>  Wed, 17 Jun 2026 17:28:43 +0100\n",
    )
    .unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "init"]);
    for branch in [
        "noble",
        "jammy",
        "focal",
        "debian/trixie",
        "debian/trixie-backports",
    ] {
        git(&["branch", branch]);
    }
    dir
}

fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not found"))
}

/// The distro-info stubs, plus git and any `extra` stubs.
fn stubs(extra: &[(&str, &str)]) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let tools = [
        (
            "ubuntu-distro-info",
            "case \"$1\" in\n\
             --all) printf 'focal\\njammy\\nnoble\\n' ;;\n\
             --supported) printf 'jammy\\nnoble\\n' ;;\n\
             esac",
        ),
        (
            "debian-distro-info",
            "case \"$*\" in\n\
             *trixie*) echo 13 ;;\n\
             *--all*) printf 'bookworm\\ntrixie\\nforky\\nsid\\n' ;;\n\
             *) exit 1 ;;\n\
             esac",
        ),
    ];
    for (name, body) in tools.iter().chain(extra) {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink(which("git"), dir.path().join("git")).unwrap();
    dir
}

/// Run dbranch with `args` in `repo`; its exit status and stdout plus
/// stderr.
fn dbranch(repo: &Path, bin: &Path, args: &[&str]) -> (bool, String) {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_dbranch"))
        .arg("--no-defaults")
        .args(args)
        .current_dir(repo)
        .env("PATH", bin)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn a_bulk_rebuild_picks_the_supported_ppa_branches_newest_first() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(&[]);
    let (ok, out) = dbranch(p, bin.path(), &["rebuild", "--dry-run"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("Bulk rebuild — 2 branch(es), newest first"),
        "{out}"
    );
    assert!(out.contains("Skipping 1 EOL release(s): focal"), "{out}");
    assert!(
        out.find("» noble").unwrap() < out.find("» jammy").unwrap(),
        "{out}"
    );
    assert!(out.contains("3.2.8-1~jammy+1"), "{out}");
    assert!(!out.contains("» focal"), "{out}");
}

#[test]
fn a_bulk_rebuild_can_include_end_of_life_releases() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(&[]);
    let (ok, out) = dbranch(
        p,
        bin.path(),
        &[
            "rebuild",
            "--dry-run",
            "--include-eol",
            "--stage",
            "all",
            "--stage",
            "tag",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("focal  (EOL)"), "{out}");
    assert!(out.contains("git push -u origin focal"), "{out}");
    assert!(out.contains("rm -f debian/files"), "{out}");
    assert!(out.contains("gbp tag"), "{out}");
}

#[test]
fn a_bulk_rebuild_refuses_what_it_cannot_do() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(&[]);
    // Uploading a bulk run needs somewhere to upload to.
    let (ok, out) = dbranch(
        p,
        bin.path(),
        &["rebuild", "--dry-run", "--stage", "upload"],
    );
    assert!(!ok);
    assert!(out.contains("the upload stage needs a target"), "{out}");
    // EOL releases cannot be uploaded to a PPA.
    let (ok, out) = dbranch(
        p,
        bin.path(),
        &[
            "rebuild",
            "--dry-run",
            "--include-eol",
            "--stage",
            "upload",
            "--ppa",
            "ppa:x/y",
        ],
    );
    assert!(!ok);
    assert!(out.contains("--include-eol can't be combined"), "{out}");
    // The merge source has to be the Debian branch.
    let (ok, out) = dbranch(
        p,
        bin.path(),
        &["rebuild", "--dry-run", "--source", "noble"],
    );
    assert!(!ok);
    assert!(out.contains("which is a PPA branch"), "{out}");
    let (ok, out) = dbranch(p, bin.path(), &["rebuild", "--dry-run", "--source", "nope"]);
    assert!(!ok);
    assert!(out.contains("source branch nope not found"), "{out}");
}

#[test]
fn a_bulk_rebuild_with_nothing_supported_says_so() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(&[]);
    for b in ["noble", "jammy"] {
        assert!(
            Command::new("git")
                .args(["branch", "-D", b])
                .current_dir(p)
                .status()
                .unwrap()
                .success()
        );
    }
    let (ok, out) = dbranch(p, bin.path(), &["rebuild", "--dry-run"]);
    assert!(!ok);
    assert!(out.contains("are EOL; pass --include-eol"), "{out}");
}

#[test]
fn named_debian_targets_are_classified_by_branch() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(&[]);
    let (ok, out) = dbranch(
        p,
        bin.path(),
        &[
            "rebuild",
            "--dry-run",
            "--stage",
            "merge",
            "debian/trixie",
            "debian/trixie-backports",
        ],
    );
    assert!(ok, "{out}");
    assert!(out.contains("deb13u1"), "{out}");
    assert!(out.contains("bpo13+1"), "{out}");
}

#[test]
fn update_dry_run_walks_every_stage() {
    let repo = repo();
    let p = &repo.path().join("damo");
    let bin = stubs(&[]);
    let (ok, out) = dbranch(
        p,
        bin.path(),
        &["update", "--dry-run", "--stage", "all", "--stage", "tag"],
    );
    assert!(ok, "{out}");
    assert!(out.contains("gbp import-orig"), "{out}");
    assert!(out.contains("debuild -S"), "{out}");
    assert!(out.contains("gbp tag"), "{out}");
}
