// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Rust wrapper for the [fedrq](https://github.com/gotmax23/fedrq) CLI tool
//! for querying Fedora and EPEL RPM repositories.

use std::fmt;
use std::path::PathBuf;
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
    Exit {
        status: std::process::ExitStatus,
        stderr: String,
    },
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

/// Return the fedrq smartcache directory (`$XDG_CACHE_HOME/fedrq`).
pub fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("HOME not set");
            PathBuf::from(home).join(".cache")
        });
    base.join("fedrq")
}

/// Check whether the fedrq smartcache for `branch` is populated.
///
/// Returns `true` if the cache directory for the branch exists and
/// contains at least one `.solv` file, meaning fedrq can serve
/// queries without downloading metadata first.
pub fn cache_fresh(branch: &str) -> bool {
    let dir = cache_dir().join(branch);
    if !dir.is_dir() {
        return false;
    }
    // Look for .solv files (compiled repo metadata).
    match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().is_some_and(|ext| ext == "solv")),
        Err(_) => false,
    }
}

/// Remove the fedrq smartcache directory so the next query fetches
/// fresh repository metadata.
pub fn clear_cache() -> std::io::Result<()> {
    let dir = cache_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

impl Fedrq {
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

    /// Return the names of all subpackages of a source package.
    pub fn subpkgs_names(&self, srpm: &str) -> Result<Vec<String>, Error> {
        self.subpkgs_query("name", srpm)
    }

    /// Return source package names that require any of the given packages.
    ///
    /// Returns an empty list if `packages` is empty (the package may not
    /// exist on this branch).
    pub fn whatrequires(&self, packages: &[String]) -> Result<Vec<String>, Error> {
        if packages.is_empty() {
            return Ok(vec![]);
        }
        let mut cmd = Command::new("fedrq");
        cmd.args(["whatrequires", "-F", "source"]);
        self.apply_opts(&mut cmd);
        cmd.args(packages);
        Self::run(&mut cmd)
    }

    /// Resolve a dependency name to the source package(s) that provide it.
    ///
    /// Uses `fedrq pkgs -P -S -F source_name <dep>` to find which source
    /// RPM provides a given capability (package name, virtual Provide, or
    /// file path).
    pub fn resolve_to_source(&self, dep: &str) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "-P", "-S", "-F", "source_name"]);
        self.apply_opts(&mut cmd);
        cmd.arg(dep);
        Self::run(&mut cmd)
    }

    /// Return the Provides of a binary package (by name).
    pub fn pkg_provides(&self, name: &str) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "-F", "provides"]);
        self.apply_opts(&mut cmd);
        cmd.arg(name);
        Self::run(&mut cmd)
    }

    /// Return the Requires of a binary package by name.
    pub fn pkg_requires(&self, name: &str) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "-F", "requires"]);
        self.apply_opts(&mut cmd);
        cmd.arg(name);
        Self::run(&mut cmd)
    }

    /// Return the Provides of packages that provide a given capability.
    ///
    /// Uses `-P` to search by Provides rather than package name.
    pub fn provides_of_provider(&self, capability: &str) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "-P", "-F", "provides"]);
        self.apply_opts(&mut cmd);
        cmd.arg(capability);
        Self::run(&mut cmd)
    }

    /// Check whether a source package exists.
    ///
    /// Equivalent to `fedrq pkgs --src <name>`.
    pub fn src_exists(&self, srpm: &str) -> Result<bool, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "--src"]);
        self.apply_opts(&mut cmd);
        cmd.arg(srpm);
        let result = Self::run(&mut cmd)?;
        Ok(result.iter().any(|s| !s.is_empty() && s != "(none)"))
    }

    /// Return the BuildRequires of a source package.
    pub fn src_buildrequires(&self, srpm: &str) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "--src", "-F", "requires"]);
        self.apply_opts(&mut cmd);
        cmd.arg(srpm);
        Self::run(&mut cmd)
    }

    /// Return the NVRs of the given source packages on this
    /// branch, in a single fedrq call.
    ///
    /// Uses fedrq's `line:name,version,release` formatter so
    /// we can pair each output row back to its input name —
    /// fedrq sorts output alphabetically and dedupes rows
    /// across repositories, so relying on input order isn't
    /// safe. The NVRs are reconstructed as
    /// `name-version-release` (no epoch), the same form that
    /// `koji list-tagged` emits.
    ///
    /// Packages not present on the branch are omitted from the
    /// result. Returns an empty vector if `packages` is empty
    /// so callers don't accidentally invoke `fedrq pkgs` with
    /// no args (which would query the entire repo).
    pub fn src_nvrs(&self, packages: &[String]) -> Result<Vec<String>, Error> {
        if packages.is_empty() {
            return Ok(vec![]);
        }
        let mut cmd = Command::new("fedrq");
        cmd.args(["pkgs", "--src", "-F", "line:name,version,release"]);
        self.apply_opts(&mut cmd);
        cmd.args(packages);
        let raw = Self::run(&mut cmd)?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for line in raw {
            let parts: Vec<&str> = line.split(" : ").collect();
            if parts.len() != 3 {
                continue;
            }
            let (name, version, release) = (parts[0].trim(), parts[1].trim(), parts[2].trim());
            if name.is_empty() || name == "(none)" {
                continue;
            }
            let nvr = format!("{name}-{version}-{release}");
            if seen.insert(nvr.clone()) {
                out.push(nvr);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_defaults() {
        let fq = Fedrq::default();
        // Verify the struct starts with no options set.
        assert!(fq.branch.is_none());
        assert!(fq.repo.is_none());
    }

    #[test]
    fn display_spawn_error() {
        let err = Error::Spawn(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "not found",
        ));
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

    #[test]
    fn cache_dir_ends_with_fedrq() {
        let dir = super::cache_dir();
        assert!(
            dir.ends_with("fedrq"),
            "cache_dir should end with 'fedrq', got: {dir:?}"
        );
    }

    #[test]
    fn cache_fresh_missing_branch() {
        assert!(!super::cache_fresh("nonexistent_test_branch_xyz"));
    }

    #[test]
    fn whatrequires_empty_packages_returns_empty() {
        let fq = Fedrq::default();
        let result = fq.whatrequires(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn src_nvrs_empty_packages_returns_empty() {
        let fq = Fedrq::default();
        let result = fq.src_nvrs(&[]).unwrap();
        assert!(result.is_empty());
    }
}
