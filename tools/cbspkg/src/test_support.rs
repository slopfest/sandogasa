// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared helpers for the subcommands' tests: fake `koji`, `cbs` and
//! friends on PATH, the workspace's scoped env-var guard ([`EnvGuard`],
//! re-exported), and a package checkout in a temp dir whose SIG remote
//! is a local bare repo, so `git fetch` and `git push` work without a
//! network.
//!
//! The module is gated on `cfg(test)` at its `main.rs` declaration.
//! Tests using [`Fixture`] change PATH, HOME and the XDG dirs, so they
//! run under `#[serial_test::serial]`.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Write a shell script named `name` in `dir` that prints the body of
/// the first case whose needle is a substring of its arguments, and
/// exits 0. A case whose body starts with `!` prints the rest to
/// stderr and exits 1 instead. Unmatched calls exit 1.
pub fn install_fake_bin(dir: &Path, name: &str, cases: &[(&str, &str)]) -> PathBuf {
    let mut script = String::from("#!/bin/sh\nargs=\"$*\"\n");
    for (needle, body) in cases {
        let (out, code) = match body.strip_prefix('!') {
            Some(err) => (format!("cat >&2 <<'__FAKE_EOF__'\n{err}\n__FAKE_EOF__"), 1),
            None => (format!("cat <<'__FAKE_EOF__'\n{body}\n__FAKE_EOF__"), 0),
        };
        script.push_str(&format!(
            "case \"$args\" in *\"{needle}\"*) {out}\nexit {code};; esac\n",
        ));
    }
    script.push_str(&format!(
        "echo \"fake-{name}: no match for: $args\" >&2\nexit 1\n",
    ));
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

pub use sandogasa_config::test_support::EnvGuard;

/// Run git in `dir`, with an identity and no signing, panicking on
/// failure; its output trimmed.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A package checkout in a temp dir, on `branch`, whose SIG remote is
/// a bare repo at a path git reads as local and the tools read as that
/// SIG's project (`…/host:CentOS/<group>/rpms/<package>.git`). Fake
/// CBS tools come first on PATH, and HOME and the XDG dirs point into
/// the temp dir, until the fixture is dropped.
pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub repo: PathBuf,
    pub bin: PathBuf,
    pub remote: String,
    _env: EnvGuard,
}

impl Fixture {
    /// `group` is the SIG's GitLab group (`CentOS/proposed_updates/rpms`),
    /// `remote` the name the SIG remote is added under, `files` what the
    /// first commit holds. The branch is pushed to the SIG remote.
    pub fn new(
        package: &str,
        branch: &str,
        group: &str,
        remote: &str,
        files: &[(&str, &str)],
    ) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let home = tmp.path().join("home");
        for d in [&bin, &home] {
            std::fs::create_dir_all(d).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let home_s = home.display().to_string();
        let config = tmp.path().join("config").display().to_string();
        let cache = tmp.path().join("cache").display().to_string();
        let state = tmp.path().join("state").display().to_string();
        let env = EnvGuard::new(&[
            ("PATH", &path),
            ("HOME", &home_s),
            ("XDG_CONFIG_HOME", &config),
            ("XDG_CACHE_HOME", &cache),
            ("XDG_STATE_HOME", &state),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("CBSPKG_LOOKASIDE_BASE", "http://127.0.0.1:9"),
            ("CBSPKG_GITLAB_BASE", "http://127.0.0.1:9"),
        ])
        .unset(&["GITLAB_TOKEN"]);
        // Every CBS tool is a fake that answers only what a test lets
        // it; anything else fails.
        install_fake_bin(&bin, "koji", &[("--help", "")]);
        install_fake_bin(&bin, "cbs", &[("--help", "")]);
        install_fake_bin(&bin, "centpkg", &[("--help", "")]);
        for name in ["centos-cert", "centos-lookaside-upload-sig", "klist"] {
            install_fake_bin(&bin, name, &[]);
        }

        let bare = tmp
            .path()
            .join(format!("host:{group}"))
            .join(format!("{package}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "--quiet", "--bare"]);
        let repo = tmp.path().join("work");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["checkout", "--quiet", "-b", branch]);
        std::fs::write(repo.join(format!("{package}.spec")), "Name: x\n").unwrap();
        for (name, body) in files {
            std::fs::write(repo.join(name), body).unwrap();
        }
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "--quiet", "-m", "first"]);
        git(
            &repo,
            &["remote", "add", remote, &bare.display().to_string()],
        );
        git(&repo, &["push", "--quiet", remote, branch]);
        Fixture {
            tmp,
            repo,
            bin,
            remote: remote.to_string(),
            _env: env,
        }
    }

    /// A Proposed Updates checkout of rsync on `branch`, its SIG remote
    /// named `sig`.
    pub fn proposed(branch: &str, files: &[(&str, &str)]) -> Self {
        Self::new(
            "rsync",
            branch,
            "CentOS/proposed_updates/rpms",
            "sig",
            files,
        )
    }

    /// A Hyperscale checkout of `package` on `branch`, its SIG remote
    /// named `origin`.
    pub fn hyperscale(package: &str, branch: &str) -> Self {
        Self::new(package, branch, "CentOS/Hyperscale/rpms", "origin", &[])
    }

    /// Replace the fake `name` with one answering `cases`.
    pub fn fake(&self, name: &str, cases: &[(&str, &str)]) {
        install_fake_bin(&self.bin, name, cases);
    }

    /// Run git in the checkout.
    pub fn git(&self, args: &[&str]) -> String {
        git(&self.repo, args)
    }

    /// Write the user's config file.
    pub fn config(&self, toml: &str) {
        let dir = self.tmp.path().join("config/cbspkg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), toml).unwrap();
    }
}
