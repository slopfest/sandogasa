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
    /// fedrq's output was not the JSON it was asked for.
    Parse(String),
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
            Error::Parse(e) => write!(f, "unexpected fedrq output: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// One package from a `fedrq … -F json:…` query.
///
/// Fields beyond `name` and `repoid` are filled only when the query
/// asked the formatter for them; the serde defaults absorb the rest.
/// `source_name` is `None` for source packages themselves.
#[derive(Debug, Clone, serde::Deserialize)]
#[non_exhaustive]
pub struct PkgInfo {
    /// Binary (or source) package name.
    pub name: String,
    /// Runtime Requires, raw dependency strings.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Provides, raw capability strings.
    #[serde(default)]
    pub provides: Vec<String>,
    /// Source package name, for a binary package.
    #[serde(default)]
    pub source_name: Option<String>,
    /// Repository the package came from (e.g. `epel`,
    /// `centos-hyperscale`, `fedrq-centos-stream-baseos`).
    #[serde(default)]
    pub repoid: String,
}

/// The XDG cache base (`$XDG_CACHE_HOME`, default `~/.cache`), via
/// the `dirs` crate — which also implements the spec's rule that a
/// *relative* `$XDG_CACHE_HOME` is invalid and must be ignored.
fn cache_base() -> PathBuf {
    dirs::cache_dir()
        .expect("cannot determine the cache directory: set XDG_CACHE_HOME (absolute) or HOME")
}

/// Return the fedrq smartcache directory (`$XDG_CACHE_HOME/fedrq`).
pub fn cache_dir() -> PathBuf {
    cache_base().join("fedrq")
}

/// Return the libdnf5 metadata cache directory
/// (`$XDG_CACHE_HOME/libdnf5`). This is the system dnf/libdnf5 cache,
/// distinct from fedrq's smartcache: queries for the *host's own*
/// Fedora release reuse it, so it can serve stale metadata for the
/// native branch even after the smartcache is cleared.
pub fn libdnf5_cache_dir() -> PathBuf {
    cache_base().join("libdnf5")
}

/// Remove the fedrq smartcache directory so the next query fetches
/// fresh repository metadata.
fn clear_cache() -> std::io::Result<()> {
    remove_if_present(&cache_dir())
}

/// Remove the libdnf5 metadata cache so the next query for the host's
/// native branch refetches fresh metadata (the smartcache clear alone
/// doesn't cover the native branch — see [`libdnf5_cache_dir`]).
fn clear_libdnf5_cache() -> std::io::Result<()> {
    remove_if_present(&libdnf5_cache_dir())
}

/// Remove both the fedrq smartcache and the libdnf5 metadata cache so
/// the next query refetches fresh metadata for every branch (native or
/// not). Use after an action that changes repo contents server-side
/// (e.g. `koji regen-repo`): clearing only the smartcache leaves
/// libdnf5 serving the pre-regen metadata for the host's native branch.
pub fn clear_all_caches() -> std::io::Result<()> {
    clear_cache()?;
    clear_libdnf5_cache()
}

fn remove_if_present(dir: &std::path::Path) -> std::io::Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
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

    fn run_raw(cmd: &mut Command) -> Result<String, Error> {
        let output = cmd.output().map_err(Error::Spawn)?;

        if !output.status.success() {
            return Err(Error::Exit {
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn run(cmd: &mut Command) -> Result<Vec<String>, Error> {
        let stdout = Self::run_raw(cmd)?;
        let lines: Vec<String> = stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        Ok(lines)
    }

    /// Run `fedrq <args>... [opts] -- <operands>...` and return the
    /// trimmed, non-empty output lines. All queries funnel through
    /// here so the branch/repo options and the `--` separator (which
    /// keeps an operand starting with `-` from being read as a flag)
    /// are applied uniformly.
    fn query<S: AsRef<std::ffi::OsStr>>(
        &self,
        args: &[&str],
        operands: &[S],
    ) -> Result<Vec<String>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(args);
        self.apply_opts(&mut cmd);
        cmd.arg("--");
        cmd.args(operands);
        Self::run(&mut cmd)
    }

    /// Run `fedrq <args>… [opts] -- <operands>…` with a `json:`
    /// formatter and parse the array it prints.
    fn query_json<S: AsRef<std::ffi::OsStr>>(
        &self,
        args: &[&str],
        operands: &[S],
    ) -> Result<Vec<PkgInfo>, Error> {
        let mut cmd = Command::new("fedrq");
        cmd.args(args);
        self.apply_opts(&mut cmd);
        cmd.arg("--");
        cmd.args(operands);
        let raw = Self::run_raw(&mut cmd)?;
        serde_json::from_str(&raw).map_err(|e| Error::Parse(e.to_string()))
    }

    /// Run `fedrq subpkgs -S -F <formatter> <srpm>` and return one entry per line.
    fn subpkgs_query(&self, formatter: &str, srpm: &str) -> Result<Vec<String>, Error> {
        self.query(&["subpkgs", "-S", "-F", formatter], &[srpm])
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

    /// Return `(name, version, release)` for every subpackage of a
    /// source package.
    ///
    /// Lets callers verify whether a given repo actually contains
    /// the expected V-R of an update (the bare `subpkgs_names`
    /// query can't distinguish old vs. new content).
    pub fn subpkgs_nvrs(&self, srpm: &str) -> Result<Vec<(String, String, String)>, Error> {
        Ok(parse_3col(
            self.subpkgs_query("line:name,version,release", srpm)?,
        ))
    }

    /// Return source package names that require any of the given packages.
    ///
    /// Returns an empty list if `packages` is empty (the package may not
    /// exist on this branch).
    pub fn whatrequires(&self, packages: &[String]) -> Result<Vec<String>, Error> {
        if packages.is_empty() {
            return Ok(vec![]);
        }
        self.query(&["whatrequires", "-F", "source"], packages)
    }

    /// Return the Requires of a *source* package — its BuildRequires —
    /// via `fedrq pkgs --src -F requires`. Complements
    /// [`Self::subpkgs_requires`] (the binary subpackages' install-time
    /// Requires): an update can break a reverse dependency's next
    /// rebuild without breaking its installed binaries.
    pub fn src_requires(&self, srpm: &str) -> Result<Vec<String>, Error> {
        self.src_buildrequires(srpm)
    }

    /// Resolve a dependency name to the source package(s) that provide it.
    ///
    /// Uses `fedrq pkgs -P -S -F source_name <dep>` to find which source
    /// RPM provides a given capability (package name, virtual Provide, or
    /// file path).
    pub fn resolve_to_source(&self, dep: &str) -> Result<Vec<String>, Error> {
        self.query(&["pkgs", "-P", "-S", "-F", "source_name"], &[dep])
    }

    /// Resolve a dependency to the source package(s) providing it, with
    /// the provider's version-release.
    ///
    /// Like [`resolve_to_source`](Self::resolve_to_source) but formatted
    /// as `line:source,version,release`, so callers can see *which*
    /// version a repo offers for a capability — e.g. to distinguish "the
    /// base distro has this package, just too old" from "absent
    /// entirely". Pass the bare capability (no version constraint) to
    /// see every offered version.
    pub fn resolve_source_vr(&self, dep: &str) -> Result<Vec<(String, String)>, Error> {
        Ok(parse_source_vr_lines(self.query(
            &["pkgs", "-P", "-S", "-F", "line:source,version,release"],
            &[dep],
        )?))
    }

    /// Return the Provides of a binary package (by name).
    pub fn pkg_provides(&self, name: &str) -> Result<Vec<String>, Error> {
        self.query(&["pkgs", "-F", "provides"], &[name])
    }

    /// Map binary package names to their source and version-release in
    /// a single query (`-F line:source,version,release`).
    ///
    /// Returns `(source_name, "version-release")` for each of `names`
    /// found. Querying `version` and `release` as separate fields keeps
    /// the release (which `nev` drops) and sidesteps `nevr`'s habit of
    /// omitting the `0:` epoch — so a stale repo differing only by
    /// release (e.g. `0.15.0-1` vs `0.15.0-3`) is still caught. Grouping
    /// by the returned source lets a caller check a whole build's
    /// binaries without guessing names from the source.
    pub fn pkgs_source_vr(&self, names: &[&str]) -> Result<Vec<(String, String)>, Error> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        Ok(parse_source_vr_lines(self.query(
            &["pkgs", "-F", "line:source,version,release"],
            names,
        )?))
    }

    /// Return the Requires of a binary package by name.
    pub fn pkg_requires(&self, name: &str) -> Result<Vec<String>, Error> {
        self.query(&["pkgs", "-F", "requires"], &[name])
    }

    /// Return the Provides of packages that provide a given capability.
    ///
    /// Uses `-P` to search by Provides rather than package name.
    pub fn provides_of_provider(&self, capability: &str) -> Result<Vec<String>, Error> {
        self.query(&["pkgs", "-P", "-F", "provides"], &[capability])
    }

    /// Batched: the binary subpackages of the given source packages,
    /// each with its runtime Requires, source attribution and
    /// providing repo — one fedrq invocation for the whole batch.
    pub fn subpkgs_info(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, Error> {
        if srpms.is_empty() {
            return Ok(vec![]);
        }
        self.query_json(
            &[
                "subpkgs",
                "-S",
                "-F",
                "json:name,requires,source_name,repoid",
            ],
            srpms,
        )
    }

    /// Batched: the given *source* packages with their Requires —
    /// which for a source package are its BuildRequires — one fedrq
    /// invocation for the whole batch. Needs source repos in the
    /// selected repo class.
    pub fn srcs_info(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, Error> {
        if srpms.is_empty() {
            return Ok(vec![]);
        }
        self.query_json(
            &[
                "pkgs",
                "--src",
                "-F",
                "json:name,requires,source_name,repoid",
            ],
            srpms,
        )
    }

    /// Batched: the packages providing the given capabilities — the
    /// candidates dnf would actually pick — with their own Requires
    /// (so a closure walk gets the next wave's inputs from the same
    /// invocation), their Provides (so the caller can attribute which
    /// capability each provider matched), source and repo.
    pub fn providers_info(&self, deps: &[String]) -> Result<Vec<PkgInfo>, Error> {
        if deps.is_empty() {
            return Ok(vec![]);
        }
        self.query_json(
            &[
                "pkgs",
                "-P",
                "-S",
                "-F",
                "json:name,requires,provides,source_name,repoid",
            ],
            deps,
        )
    }

    /// Check whether a source package exists.
    ///
    /// Equivalent to `fedrq pkgs --src <name>`.
    pub fn src_exists(&self, srpm: &str) -> Result<bool, Error> {
        let result = self.query(&["pkgs", "--src"], &[srpm])?;
        Ok(result.iter().any(|s| !s.is_empty() && s != "(none)"))
    }

    /// Return the BuildRequires of a source package.
    pub fn src_buildrequires(&self, srpm: &str) -> Result<Vec<String>, Error> {
        self.query(&["pkgs", "--src", "-F", "requires"], &[srpm])
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
        let raw = self.query(
            &["pkgs", "--src", "-F", "line:name,version,release"],
            packages,
        )?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (name, version, release) in parse_3col(raw) {
            let nvr = format!("{name}-{version}-{release}");
            if seen.insert(nvr.clone()) {
                out.push(nvr);
            }
        }
        Ok(out)
    }
}

/// Parse fedrq's three-column `line:` output (columns separated by
/// ` : `) into trimmed triples, dropping malformed lines and rows
/// whose first column is empty or `(none)`.
fn parse_3col(raw: Vec<String>) -> Vec<(String, String, String)> {
    raw.iter()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split(" : ").collect();
            let [a, b, c] = parts[..] else { return None };
            let (a, b, c) = (a.trim(), b.trim(), c.trim());
            if a.is_empty() || a == "(none)" {
                return None;
            }
            Some((a.to_string(), b.to_string(), c.to_string()))
        })
        .collect()
}

/// Parse `line:source,version,release` output into
/// `(source, "version-release")` pairs, dropping `(none)` and malformed
/// lines. Shared by [`Fedrq::pkgs_source_vr`] and
/// [`Fedrq::resolve_source_vr`].
fn parse_source_vr_lines(raw: Vec<String>) -> Vec<(String, String)> {
    parse_3col(raw)
        .into_iter()
        .filter(|(_, version, release)| !version.is_empty() && !release.is_empty())
        .map(|(source, version, release)| (source, format!("{version}-{release}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_info_parses_a_full_record() {
        let raw = r#"[{"name":"htop","requires":["libc.so.6()(64bit)"],
            "provides":["htop = 3.3.0-1.el9"],
            "source_name":"htop","repoid":"epel"}]"#;
        let pkgs: Vec<PkgInfo> = serde_json::from_str(raw).unwrap();
        assert_eq!(pkgs[0].name, "htop");
        assert_eq!(pkgs[0].requires, ["libc.so.6()(64bit)"]);
        assert_eq!(pkgs[0].source_name.as_deref(), Some("htop"));
        assert_eq!(pkgs[0].repoid, "epel");
    }

    #[test]
    fn pkg_info_defaults_absorb_unrequested_attrs() {
        // A query that asked only for name,repoid — the other
        // fields must default rather than fail the parse.
        let raw = r#"[{"name":"htop","repoid":"epel"}]"#;
        let pkgs: Vec<PkgInfo> = serde_json::from_str(raw).unwrap();
        assert!(pkgs[0].requires.is_empty());
        assert!(pkgs[0].provides.is_empty());
        assert!(pkgs[0].source_name.is_none());
    }

    #[test]
    fn empty_inputs_skip_the_spawn() {
        let f = Fedrq::default();
        assert!(f.subpkgs_info(&[]).unwrap().is_empty());
        assert!(f.providers_info(&[]).unwrap().is_empty());
    }

    #[test]
    fn build_command_defaults() {
        let fq = Fedrq::default();
        // Verify the struct starts with no options set.
        assert!(fq.branch.is_none());
        assert!(fq.repo.is_none());
    }

    #[test]
    fn parse_source_vr_lines_extracts_pairs() {
        let raw = vec![
            "python-setuptools : 69.0.3 : 9.el10".to_string(),
            "(none) : 1 : 1".to_string(),
            "garbage line".to_string(),
            "rust-foo : 1.2.3 : 1.fc45".to_string(),
        ];
        assert_eq!(
            parse_source_vr_lines(raw),
            vec![
                ("python-setuptools".to_string(), "69.0.3-9.el10".to_string()),
                ("rust-foo".to_string(), "1.2.3-1.fc45".to_string()),
            ]
        );
    }

    #[test]
    fn cache_dirs_are_siblings_under_one_base() {
        // The fedrq smartcache and the libdnf5 cache live side by side
        // under the same XDG cache base (so `--refresh` clears both).
        let fedrq = cache_dir();
        let libdnf5 = libdnf5_cache_dir();
        assert_eq!(fedrq.file_name().unwrap(), "fedrq");
        assert_eq!(libdnf5.file_name().unwrap(), "libdnf5");
        assert_eq!(fedrq.parent(), libdnf5.parent());
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
