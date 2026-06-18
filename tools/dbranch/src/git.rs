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

/// Whether an executable is on `PATH`.
pub fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let p = dir.join(name);
                p.is_file()
            })
        })
        .unwrap_or(false)
}

/// Verify the external tools the selected stages need are installed,
/// with an actionable message naming the providing package when one
/// is missing. `git` is always required; the rest are per-stage.
pub fn ensure_tools(
    need_gbp: bool,
    need_build: bool,
    need_lint: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // (executable, providing package)
    let mut required: Vec<(&str, &str)> = vec![("git", "git")];
    if need_gbp {
        required.push(("gbp", "git-buildpackage"));
    }
    if need_build {
        required.push(("debuild", "devscripts"));
        required.push(("pbuilder-dist", "ubuntu-dev-tools"));
    }
    if need_lint {
        required.push(("lintian", "lintian"));
    }
    let missing: Vec<String> = required
        .iter()
        .filter(|(exe, _)| !tool_exists(exe))
        .map(|(exe, pkg)| format!("{exe} (install: {pkg})"))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing required tool(s): {}", missing.join(", ")).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_exists_detects_present_and_absent() {
        // `sh` is on PATH in any dev/CI environment.
        assert!(tool_exists("sh"));
        assert!(!tool_exists("definitely-not-a-real-tool-xyzzy"));
    }
}
