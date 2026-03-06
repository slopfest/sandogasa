// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fmt;
use std::process::Command;

/// Options for fedrq queries.
#[derive(Clone, Debug, Default)]
pub struct Fedrq {
    /// Branch/release to query (e.g. "rawhide", "f41", "epel9").
    pub branch: Option<String>,
    /// Repository class (e.g. "@base", "@testing").
    pub repo: Option<String>,
}

/// Errors from fedrq invocations.
#[derive(Debug)]
pub enum Error {
    /// Failed to spawn the fedrq process.
    Spawn(std::io::Error),
    /// fedrq exited with a non-zero status.
    Exit { status: std::process::ExitStatus, stderr: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Spawn(e) => write!(f, "failed to run fedrq: {e}"),
            Error::Exit { status, stderr } => {
                write!(f, "fedrq exited with {status}")?;
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {}

impl Fedrq {
    pub fn new() -> Self {
        Self::default()
    }

    fn apply_opts(&self, cmd: &mut Command) {
        if let Some(branch) = &self.branch {
            cmd.args(["-b", branch]);
        }
        if let Some(repo) = &self.repo {
            cmd.args(["-r", repo]);
        }
    }

    fn run(cmd: &mut Command) -> Result<Vec<String>, Error> {
        let output = cmd.output().map_err(Error::Spawn)?;

        if !output.status.success() {
            return Err(Error::Exit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<String> = stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        Ok(lines)
    }

    /// Run `fedrq pkgs -F <formatter> <package>` and return one entry per line.
    fn pkgs_query(&self, formatter: &str, package: &str) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "-F", formatter]);

        self.apply_opts(&mut cmd);
        cmd.arg(package);
        Self::run(&mut cmd)
    }

    /// Return the Requires (dependencies) of a package.
    pub fn requires(&self, package: &str) -> Result<Vec<String>, Error> {
        self.pkgs_query("requires", package)
    }

    /// Return the Provides of a package.
    pub fn provides(&self, package: &str) -> Result<Vec<String>, Error> {
        self.pkgs_query("provides", package)
    }

    /// Run `fedrq subpkgs -S -F <formatter> <srpm>` and return one entry per line.
    fn subpkgs_query(&self, formatter: &str, srpm: &str) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["subpkgs", "-S", "-F", formatter]);
        self.apply_opts(&mut cmd);
        cmd.arg(srpm);
        Self::run(&mut cmd)
    }

    /// Return the Provides of all subpackages of a source package.
    pub fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, Error> {
        self.subpkgs_query("provides", srpm)
    }

    /// Return the Requires of all subpackages of a source package.
    pub fn subpkgs_requires(&self, srpm: &str) -> Result<Vec<String>, Error> {
        self.subpkgs_query("requires", srpm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_defaults() {
        let fq = Fedrq::new();
        // Verify the struct starts with no options set.
        assert!(fq.branch.is_none());
        assert!(fq.repo.is_none());
    }

    #[test]
    fn display_spawn_error() {
        let err = Error::Spawn(std::io::Error::new(std::io::ErrorKind::NotFound, "not found"));
        let msg = err.to_string();
        assert!(msg.contains("failed to run fedrq"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn display_exit_error_with_stderr() {
        let status = Command::new("false").status().expect("can run false");
        let err = Error::Exit {
            status,
            stderr: "package not found".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("fedrq exited with"));
        assert!(msg.contains("package not found"));
    }

    #[test]
    fn display_exit_error_empty_stderr() {
        let status = Command::new("false").status().expect("can run false");
        let err = Error::Exit {
            status,
            stderr: String::new(),
        };
        let msg = err.to_string();
        assert!(msg.contains("fedrq exited with"));
        assert!(!msg.contains("package not found"));
    }
}
