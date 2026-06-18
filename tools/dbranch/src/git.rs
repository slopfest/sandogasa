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

/// Verify the external tools the selected stages need are installed,
/// with an actionable message naming the providing package when one
/// is missing. `git` is always required; the rest are per-stage.
pub fn ensure_tools(
    need_gbp: bool,
    need_build: bool,
    need_lint: bool,
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
    sandogasa_cli::require_tools(&required).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_tools_ok_for_present_basics() {
        // git is present in dev/CI; with no extra stages this passes.
        assert!(ensure_tools(false, false, false).is_ok());
    }
}
