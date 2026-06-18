// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small git query helpers (read-only, not narrated — they gather
//! state, they don't perform the workflow) plus external-tool
//! availability checks.

use std::path::Path;
use std::process::Command;

/// List local branch names in `repo`.
pub fn local_branches(repo: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
        .current_dir(repo)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// The currently checked-out branch in `repo` — the Debian branch
/// dbranch merges from. Errors on a detached HEAD.
pub fn current_branch(repo: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(repo)
        .output()?;
    if !out.status.success() {
        return Err("not on a branch (detached HEAD?); check out the Debian branch first".into());
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        return Err("could not determine the current branch".into());
    }
    Ok(name)
}

/// Paths left unmerged after a merge (conflict-filter `U`).
pub fn unmerged_paths(repo: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(repo)
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Read a file's contents from a branch without checking it out
/// (`git show <branch>:<path>`). `None` if the path doesn't exist
/// there.
pub fn show_file(repo: &Path, branch: &str, path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("show")
        .arg(format!("{branch}:{path}"))
        .current_dir(repo)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Whether `branch` has an upstream configured (`<branch>@{upstream}`
/// resolves). Lets the push stage drop the `-u origin <branch>` once
/// tracking is set and just run `git push`.
pub fn has_upstream(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "rev-parse",
            "--abbrev-ref",
            &format!("{branch}@{{upstream}}"),
        ])
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether `<remote>/<branch>` exists as a remote-tracking ref
/// (`refs/remotes/<remote>/<branch>`) — i.e. the branch is on the
/// remote and has been fetched, even if it was never checked out
/// locally. Lets dbranch distinguish "branch I haven't created locally
/// yet" from "brand-new branch", so it tracks the existing remote one
/// instead of recreating it from the Debian branch.
pub fn remote_branch_exists(repo: &Path, remote: &str, branch: &str) -> bool {
    Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote}/{branch}"),
        ])
        .current_dir(repo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve a revision to its full commit SHA (`git rev-parse <rev>`).
/// `None` if it can't be resolved (unknown ref, etc.).
pub fn rev_parse(repo: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// The host of a remote's URL (e.g. `origin` →
/// `git@salsa.debian.org:…` → `salsa.debian.org`). `None` if the
/// remote is unset or the URL can't be parsed.
pub fn remote_host(repo: &Path, remote: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", remote])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    crate::plan::host_from_remote_url(&url)
}

/// Verify glab has a token for `host` (glab stores one per host), with
/// the exact `glab auth login` command to run if not. Checked before
/// the push stage's CI watch / `watch-ci` so an unauthenticated glab
/// fails up front with a clear remedy instead of a downstream API
/// error. glab's own output is captured and only surfaced on failure —
/// older glab (e.g. 1.53) prints a spurious "Invalid token" warning on
/// a working token, which we'd otherwise dump on every run.
pub fn ensure_glab_auth(repo: &Path, host: &str) -> Result<(), Box<dyn std::error::Error>> {
    let out = Command::new("glab")
        .args(["auth", "status", "--hostname", host])
        .current_dir(repo)
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&out.stderr);
    let detail = detail.trim();
    let remedy = format!("run: glab auth login --hostname {host}");
    if detail.is_empty() {
        Err(format!("glab is not authenticated to {host}; {remedy}").into())
    } else {
        Err(format!("glab is not authenticated to {host}: {detail}\n{remedy}").into())
    }
}

/// Verify the external tools the selected stages need are installed,
/// with an actionable message naming the providing package when one
/// is missing. `git` is always required; the rest are per-stage.
pub fn ensure_tools(
    need_gbp: bool,
    need_build: bool,
    need_lint: bool,
    need_glab: bool,
    need_upload: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // (executable, providing package, version/help probe). Most tools
    // answer `--version`; pbuilder-dist doesn't, but `--help` exits 0.
    let mut required: Vec<(&str, &str, Option<&str>)> = vec![("git", "git", Some("--version"))];
    if need_gbp {
        required.push(("gbp", "git-buildpackage", Some("--version")));
    }
    if need_build {
        required.push(("debuild", "devscripts", Some("--version")));
        required.push(("pbuilder-dist", "ubuntu-dev-tools", Some("--help")));
    }
    if need_lint {
        required.push(("lintian", "lintian", Some("--version")));
    }
    if need_glab {
        required.push(("glab", "apt install glab (GitLab CLI)", Some("--version")));
    }
    if need_upload {
        // Probe by $PATH only — dput's `--version` support varies
        // between dput and dput-ng.
        required.push(("dput", "dput", None));
    }
    sandogasa_cli::require_tools(&required).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_tools_ok_for_present_basics() {
        // git is present in dev/CI; with no extra stages this passes.
        assert!(ensure_tools(false, false, false, false, false).is_ok());
    }

    #[test]
    fn remote_host_reads_origin_url() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
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
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "git@salsa.debian.org:python-team/packages/damo.git",
        ]);
        assert_eq!(
            remote_host(p, "origin").as_deref(),
            Some("salsa.debian.org")
        );
        // An unknown remote yields None rather than erroring.
        assert_eq!(remote_host(p, "nope"), None);
    }

    #[test]
    fn has_upstream_reflects_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(p)
                    .env("GIT_AUTHOR_NAME", "T")
                    .env("GIT_AUTHOR_EMAIL", "t@x")
                    .env("GIT_COMMITTER_NAME", "T")
                    .env("GIT_COMMITTER_EMAIL", "t@x")
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("f"), "x").unwrap();
        git(&["add", "-A"]);
        git(&["-c", "commit.gpgsign=false", "commit", "-qm", "init"]);
        assert!(!has_upstream(p, "main"));
        // Fake a fetched remote branch and set main to track it. The
        // remote (with its fetch refspec) is needed for @{upstream} to
        // recognize the ref as a remote-tracking branch.
        git(&["remote", "add", "origin", "/tmp/dbranch-test.git"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["config", "branch.main.remote", "origin"]);
        git(&["config", "branch.main.merge", "refs/heads/main"]);
        assert!(has_upstream(p, "main"));
    }
}
