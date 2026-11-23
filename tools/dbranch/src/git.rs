// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small git query helpers (read-only, not narrated — they gather
//! state, they don't perform the workflow) plus external-tool
//! availability checks.

use std::path::Path;
use std::process::Command;

/// List local branch names in `repo`.
pub fn local_branches(repo: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        // `%(refname)` and strip, rather than `%(refname:short)`:
        // `:short` gives the shortest *unambiguous* name, so in a
        // repository cloned from upstream's git -- remote `upstream`
        // by convention -- a local branch called `upstream` comes back
        // as `heads/upstream` and matches nothing it is compared with.
        .args(["for-each-ref", "--format=%(refname)", "refs/heads/"])
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
        .map(|l| {
            let l = l.trim();
            l.strip_prefix("refs/heads/").unwrap_or(l).to_string()
        })
        .filter(|l| !l.is_empty())
        .collect())
}

/// The branch a remote's HEAD points at — upstream's mainline, which
/// `debian/gbp.conf` must name as `upstream-branch` because gbp's
/// default is a branch called `upstream` that this layout has no
/// reason to create. `None` when the remote has no HEAD recorded
/// (a clone made with `--no-tags`, or a remote added by hand).
pub fn remote_head(repo: &Path, remote: &str) -> Option<String> {
    let out = Command::new("git")
        .args([
            "symbolic-ref",
            "--quiet",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let full = String::from_utf8_lossy(&out.stdout).trim().to_string();
    full.strip_prefix(&format!("{remote}/")).map(str::to_string)
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

/// The committer date of `rev` as seconds since the epoch
/// (`git log -1 --format=%ct <rev>`) — when that tree state came into
/// being, which is what a build artifact's mtime is compared against.
/// `None` if the revision can't be resolved (an empty repository, etc.).
pub fn commit_time(repo: &Path, rev: &str) -> Option<u64> {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%ct", rev])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The host of a remote's URL (e.g. `origin` →
/// `git@salsa.debian.org:…` → `salsa.debian.org`). `None` if the
/// remote is unset or the URL can't be parsed.
pub fn remote_host(repo: &Path, remote: &str) -> Option<String> {
    crate::plan::host_from_remote_url(&remote_url(repo, remote)?)
}

/// A remote's URL (`git remote get-url <remote>`).
pub fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["remote", "get-url", remote])
        .current_dir(repo)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Tags matching `glob` (all tags when `None`), newest first by
/// version order (`git tag --list --sort=-version:refname`, so `v0.10`
/// sorts above `v0.9`).
pub fn tags_by_version(repo: &Path, glob: Option<&str>) -> Vec<String> {
    let mut cmd = Command::new("git");
    cmd.args(["tag", "--list", "--sort=-version:refname"]);
    if let Some(g) = glob {
        cmd.arg(g);
    }
    cmd.current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `rev` is an ancestor of (or equal to) `of`
/// (`git merge-base --is-ancestor`).
pub fn is_ancestor(repo: &Path, rev: &str, of: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", rev, of])
        .current_dir(repo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// How many commits `head` has that `base` has not
/// (`git rev-list --count base..head -- <path>`). `None` when either
/// is unknown to this repository.
///
/// `path` is what makes the number mean anything when the branches
/// share an upstream: merging a release tag brings all of upstream's
/// code commits into the range, so comparing two packaging branches
/// without limiting to `debian` counts their work as our divergence.
/// On fnlfmt that was 19 commits against the 1 that was ours.
pub fn commits_ahead(repo: &Path, base: &str, head: &str, path: Option<&str>) -> Option<usize> {
    let range = format!("{base}..{head}");
    let mut args = vec!["rev-list", "--count", &range];
    if let Some(path) = path {
        args.push("--");
        args.push(path);
    }
    let out = Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}

/// Whether the working tree differs from `HEAD`
/// (`git diff --quiet`, inverted).
///
/// How a reformatting run is told from a no-op: `wrap-and-sort`
/// rewrites in place and says nothing about whether it changed
/// anything, so the tree is what answers.
pub fn is_dirty(repo: &Path) -> bool {
    Command::new("git")
        .args(["diff", "--quiet"])
        .current_dir(repo)
        .status()
        .map(|s| !s.success())
        .unwrap_or(false)
}

/// What `debuild -S` leaves in the tree. It is ignored when deciding
/// whether a tree is [`pristine`], so a second source build of the
/// same tree still counts as clean.
pub const SOURCE_BUILD_LEFTOVER: &str = "debian/files";

/// Whether `git status --porcelain --ignored --untracked-files=all`
/// output describes a tree that holds exactly what `HEAD` does, apart
/// from [`SOURCE_BUILD_LEFTOVER`].
pub fn pristine_status(porcelain: &str) -> bool {
    porcelain
        .lines()
        .filter(|l| !l.is_empty())
        // Untracked or ignored only: a tracked debian/files that was
        // changed or deleted is a real change, and removing it would
        // dirty the tree.
        .all(|l| {
            (l.starts_with("?? ") || l.starts_with("!! "))
                && l.get(3..) == Some(SOURCE_BUILD_LEFTOVER)
        })
}

/// Whether the work tree holds exactly what `HEAD` does: nothing
/// modified, staged, untracked or ignored, apart from
/// [`SOURCE_BUILD_LEFTOVER`].
///
/// Such a tree has nothing for `debian/rules clean` to remove, so a
/// source build can skip it (`-nc`). The clean target loads
/// `debian/rules`, which can need build-deps the host does not have.
/// Ignored files count, since a clean would remove build products
/// that `.gitignore` hides. When git cannot answer, the tree is taken
/// as not pristine, which keeps the clean.
pub fn pristine(repo: &Path) -> bool {
    Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--ignored",
            "--untracked-files=all",
        ])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| pristine_status(&String::from_utf8_lossy(&o.stdout)))
}

/// A remote's branches, as plain names (`debian/latest`), sorted.
pub fn remote_branches(repo: &Path, remote: &str) -> Vec<String> {
    Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname:strip=3)",
            &format!("refs/remotes/{remote}/"),
        ])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| *l != "HEAD")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `rev`'s tree has `path` (`git ls-tree <rev> -- <path>`).
pub fn tree_has_path(repo: &Path, rev: &str, path: &str) -> bool {
    Command::new("git")
        .args(["ls-tree", rev, "--", path])
        .current_dir(repo)
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// `<author> (<date>)` of a revision's commit, for describing it.
pub fn commit_author_date(repo: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%an (%as)", rev])
        .current_dir(repo)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The configured remotes (`git remote`), in git's (sorted) order.
pub fn remotes(repo: &Path) -> Vec<String> {
    Command::new("git")
        .arg("remote")
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The remote a branch is configured to push to: `branch.<b>.pushRemote`
/// if set, else `branch.<b>.remote` (its upstream). `None` when neither
/// is set or the branch tracks a local one (`.`).
pub fn branch_remote(repo: &Path, branch: &str) -> Option<String> {
    ["pushRemote", "remote"].iter().find_map(|key| {
        let out = Command::new("git")
            .args(["config", "--get", &format!("branch.{branch}.{key}")])
            .current_dir(repo)
            .output()
            .ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !v.is_empty() && v != ".").then_some(v)
    })
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
#[allow(clippy::too_many_arguments)]
pub fn ensure_tools(
    need_gbp: bool,
    need_build: bool,
    need_lint: bool,
    need_glab: bool,
    need_upload: bool,
    need_tag: bool,
    need_import: bool,
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
    if need_tag {
        // Probe by $PATH only — `dh` has no reliable side-effect-free
        // version flag (neither `--version` nor `--help` exits 0).
        required.push(("dh", "debhelper", None));
    }
    if need_import {
        // `gbp import-orig --uscan --pristine-tar` needs both.
        required.push(("uscan", "devscripts", Some("--version")));
        required.push(("pristine-tar", "pristine-tar", None));
    }
    sandogasa_cli::require_tools(&required).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_tools_ok_for_present_basics() {
        // git is present in dev/CI; with no extra stages this passes.
        assert!(ensure_tools(false, false, false, false, false, false, false).is_ok());
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
    fn only_the_source_builds_leftover_keeps_a_tree_pristine() {
        assert!(pristine_status(""));
        assert!(pristine_status("!! debian/files\n"));
        assert!(pristine_status("?? debian/files\n"));
        assert!(!pristine_status(" M debian/control\n"));
        assert!(!pristine_status(" M debian/files\n"));
        assert!(!pristine_status("D  debian/files\n"));
        assert!(!pristine_status("?? debian/files\n?? notes.txt\n"));
        // A build product .gitignore hides is still something a clean
        // would remove.
        assert!(!pristine_status("!! debian/.debhelper/x\n"));
        assert!(!pristine_status("!! debian/files.new\n"));
    }

    #[test]
    fn pristine_reads_the_real_tree() {
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
        git(&["config", "user.email", "t@example.org"]);
        git(&["config", "user.name", "T"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir(p.join("debian")).unwrap();
        std::fs::write(p.join("debian/control"), "Source: x\n").unwrap();
        std::fs::write(p.join(".gitignore"), "*.o\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        assert!(pristine(p));
        std::fs::write(p.join("debian/files"), "x_1.dsc devel optional\n").unwrap();
        assert!(pristine(p));
        std::fs::write(p.join("a.o"), "").unwrap();
        assert!(!pristine(p), "an ignored build product is not pristine");
        std::fs::remove_file(p.join("a.o")).unwrap();
        std::fs::write(p.join("debian/control"), "Source: y\n").unwrap();
        assert!(!pristine(p));
        // Not a repository: keep the clean.
        let empty = tempfile::tempdir().unwrap();
        assert!(!pristine(empty.path()));
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

        // Remote helpers on the same fixture.
        assert_eq!(
            remote_url(p, "origin").as_deref(),
            Some("/tmp/dbranch-test.git")
        );
        assert_eq!(remote_url(p, "nope"), None);
        assert_eq!(branch_remote(p, "main").as_deref(), Some("origin"));
        assert_eq!(branch_remote(p, "other"), None);
        git(&["remote", "add", "fork", "/tmp/dbranch-fork.git"]);
        assert_eq!(remotes(p), ["fork", "origin"]);
        // pushRemote wins over the upstream's remote.
        git(&["config", "branch.main.pushRemote", "fork"]);
        assert_eq!(branch_remote(p, "main").as_deref(), Some("fork"));
    }
}
