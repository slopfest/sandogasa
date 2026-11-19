// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The repositories a check reads, behind [`RepoQuery`], with fedrq's
//! view as the real one.

use std::collections::BTreeSet;

use rayon::prelude::*;
use sandogasa_cli::progress::Progress;

/// One repository view a check reads: a branch, a repo class, or both.
///
/// Everything a check reads goes through it: which source packages and
/// binaries are there, what they provide and require, and who requires
/// or provides a capability. fedrq's view implements it; a tool can
/// supply another, for builds no repository holds yet.
pub trait RepoQuery: Sync {
    /// The repositories the view resolves to, which is also how a
    /// check proves the branch and repo class exist.
    fn repolist(&self) -> Result<Vec<String>, String>;

    /// The NVRs of the given source packages present in the view.
    fn src_nvrs(&self, packages: &[String]) -> Result<Vec<String>, String>;

    /// The binary package names a source package built here.
    fn subpkgs_names(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// `(name, version, release)` for each binary package of a source.
    fn subpkgs_nvrs(&self, srpm: &str) -> Result<Vec<(String, String, String)>, String>;

    /// `(source, version-release)` for each named binary package.
    fn pkgs_source_vr(&self, names: &[&str]) -> Result<Vec<(String, String)>, String>;

    /// Everything the subpackages of a source declare.
    fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// Everything one binary package declares.
    fn pkg_provides(&self, name: &str) -> Result<Vec<String>, String>;

    /// What the package providing a capability declares, which is how
    /// a changed Provide is traced back to what replaced it.
    fn provides_of_provider(&self, capability: &str) -> Result<Vec<String>, String>;

    /// What one binary package requires.
    fn pkg_requires(&self, name: &str) -> Result<Vec<String>, String>;

    /// What the subpackages of a source require.
    fn subpkgs_requires(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// What a source package requires: its BuildRequires.
    fn src_requires(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// The source packages requiring any of these capabilities.
    fn whatrequires(&self, packages: &[String]) -> Result<Vec<String>, String>;

    /// The source packages providing a capability, constraint included.
    ///
    /// The constraint is passed through rather than stripped, so the
    /// repository decides whether a version satisfies it. Repeating
    /// RPM's comparison here would be a second implementation to keep
    /// right.
    fn resolve_to_source(&self, dep: &str) -> Result<Vec<String>, String>;

    // Batched forms of the queries above, for the phases that ask
    // about hundreds of packages or thousands of capabilities. Each
    // answers what the one-at-a-time query would, an error counting as
    // an empty answer as there; the defaults ask one at a time, and
    // fedrq's view answers a whole batch in one invocation.

    /// Each named binary package's Requires.
    fn pkgs_requires(&self, names: &[String]) -> Vec<(String, Vec<String>)> {
        names
            .iter()
            .map(|n| {
                (
                    n.clone(),
                    filter_none(self.pkg_requires(n).unwrap_or_default()),
                )
            })
            .collect()
    }

    /// Everything the named binary packages provide.
    fn pkgs_provides(&self, names: &[String]) -> Vec<String> {
        names
            .iter()
            .flat_map(|n| filter_none(self.pkg_provides(n).unwrap_or_default()))
            .collect()
    }

    /// Everything the subpackages of these sources provide.
    fn subpkgs_provides_all(&self, srpms: &[String]) -> Vec<String> {
        srpms
            .iter()
            .flat_map(|s| filter_none(self.subpkgs_provides(s).unwrap_or_default()))
            .collect()
    }

    /// Which of `caps` (bare capability names, or file paths) some
    /// package in the view provides.
    fn resolvable(&self, caps: &[String]) -> BTreeSet<String> {
        caps.iter()
            .filter(|c| !filter_none(self.provides_of_provider(c).unwrap_or_default()).is_empty())
            .cloned()
            .collect()
    }

    /// For each source: its subpackages' Requires, and its own
    /// BuildRequires.
    fn sources_requires(&self, srpms: &[String]) -> Vec<(String, Vec<String>, Vec<String>)> {
        srpms
            .iter()
            .map(|s| {
                (
                    s.clone(),
                    filter_none(self.subpkgs_requires(s).unwrap_or_default()),
                    filter_none(self.src_requires(s).unwrap_or_default()),
                )
            })
            .collect()
    }
}

/// How many names one batched fedrq query carries: enough that a large
/// update takes a handful of invocations, few enough that the command
/// line stays far below the kernel's limit.
pub const QUERY_CHUNK: usize = 250;

/// Run `per_chunk` over `items` a chunk at a time, in parallel, with a
/// counter that ticks once per chunk.
pub fn chunked<T: Send>(
    items: &[String],
    label: &str,
    verbose: bool,
    per_chunk: impl Fn(&[String]) -> Vec<T> + Sync + Send,
) -> Vec<T> {
    let progress = Progress::stderr(label, items.len().div_ceil(QUERY_CHUNK), verbose);
    items
        .par_chunks(QUERY_CHUNK)
        .flat_map_iter(|chunk| {
            progress.tick(&chunk[0]);
            per_chunk(chunk)
        })
        .collect()
}

/// Which of `caps` (bare capability names) one of `providers` provides,
/// by name: the providers a batched `-P` query returned for all of
/// them together, so each has to be matched back to its capability.
pub fn resolved_by(caps: &[String], providers: &[sandogasa_fedrq::PkgInfo]) -> BTreeSet<String> {
    caps.iter()
        .filter(|c| providers.iter().any(|p| p.satisfies(c)))
        .cloned()
        .collect()
}

/// Which of `paths` one of `providers` declares, or ships under any
/// path the merges give it (`/sbin/nologin` is shipped as
/// `/usr/bin/nologin`).
pub fn paths_shipped(paths: &[String], providers: &[sandogasa_fedrq::PkgInfo]) -> BTreeSet<String> {
    let met: BTreeSet<&str> = providers
        .iter()
        .flat_map(|p| p.files.iter().chain(&p.provides).map(String::as_str))
        .collect();
    paths
        .iter()
        .filter(|p| {
            sandogasa_fedrq::merged_paths(p)
                .iter()
                .any(|m| met.contains(m.as_str()))
        })
        .cloned()
        .collect()
}

/// Filter out fedrq's "(none)" placeholder from results.
pub fn filter_none(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .filter(|s| !s.is_empty() && s != "(none)")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository answering the one-at-a-time queries from fixed
    /// tables, so the batched defaults can be checked against them.
    struct Table;

    impl RepoQuery for Table {
        fn repolist(&self) -> Result<Vec<String>, String> {
            Ok(vec!["baseos".into()])
        }
        fn src_nvrs(&self, _: &[String]) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn subpkgs_names(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn subpkgs_nvrs(&self, _: &str) -> Result<Vec<(String, String, String)>, String> {
            Ok(vec![])
        }
        fn pkgs_source_vr(&self, _: &[&str]) -> Result<Vec<(String, String)>, String> {
            Ok(vec![])
        }
        fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, String> {
            match srpm {
                "foo" => Ok(vec!["foo = 1".into(), "(none)".into()]),
                _ => Err("no such source".into()),
            }
        }
        fn pkg_provides(&self, name: &str) -> Result<Vec<String>, String> {
            match name {
                "foo-libs" => Ok(vec!["libfoo.so.1()(64bit)".into(), String::new()]),
                _ => Err("no such package".into()),
            }
        }
        fn provides_of_provider(&self, capability: &str) -> Result<Vec<String>, String> {
            match capability {
                "libfoo.so.1()(64bit)" => Ok(vec!["libfoo.so.1()(64bit)".into()]),
                "gone" => Ok(vec!["(none)".into()]),
                _ => Err("no provider".into()),
            }
        }
        fn pkg_requires(&self, name: &str) -> Result<Vec<String>, String> {
            match name {
                "foo-libs" => Ok(vec!["glibc".into(), "(none)".into()]),
                _ => Err("no such package".into()),
            }
        }
        fn subpkgs_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
            match srpm {
                "bar" => Ok(vec!["libfoo.so.1()(64bit)".into()]),
                _ => Err("no such source".into()),
            }
        }
        fn src_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
            match srpm {
                "bar" => Ok(vec!["foo-devel".into(), "(none)".into()]),
                _ => Ok(vec![]),
            }
        }
        fn whatrequires(&self, _: &[String]) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn resolve_to_source(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_batched_defaults_answer_as_the_single_queries_do() {
        let repo = Table;
        // An error, fedrq's "(none)" and an empty line count as nothing.
        assert_eq!(
            repo.pkgs_requires(&names(&["foo-libs", "missing"])),
            [
                ("foo-libs".to_string(), names(&["glibc"])),
                ("missing".to_string(), vec![])
            ]
        );
        assert_eq!(
            repo.pkgs_provides(&names(&["foo-libs", "missing"])),
            ["libfoo.so.1()(64bit)"]
        );
        assert_eq!(
            repo.subpkgs_provides_all(&names(&["foo", "missing"])),
            ["foo = 1"]
        );
        assert_eq!(
            repo.resolvable(&names(&["libfoo.so.1()(64bit)", "gone", "nowhere"])),
            BTreeSet::from(["libfoo.so.1()(64bit)".to_string()])
        );
        assert_eq!(
            repo.sources_requires(&names(&["bar", "baz"])),
            [
                (
                    "bar".to_string(),
                    names(&["libfoo.so.1()(64bit)"]),
                    names(&["foo-devel"])
                ),
                ("baz".to_string(), vec![], vec![])
            ]
        );
        assert_eq!(repo.repolist().unwrap(), ["baseos"]);
    }

    #[test]
    fn chunked_covers_every_item_in_order() {
        let items: Vec<String> = (0..QUERY_CHUNK * 2 + 3).map(|i| i.to_string()).collect();
        let lengths = chunked(&items, "counting", false, |chunk| vec![chunk.len()]);
        assert_eq!(lengths, [QUERY_CHUNK, QUERY_CHUNK, 3]);
        let echoed = chunked(&items, "echoing", false, |chunk| chunk.to_vec());
        assert_eq!(echoed, items);
        assert!(chunked(&[], "nothing", false, |c| c.to_vec()).is_empty());
    }

    #[test]
    fn filter_none_drops_placeholders_and_blanks() {
        assert_eq!(filter_none(names(&["a", "(none)", "", "b"])), ["a", "b"]);
    }

    /// A package as a batched fedrq query returns it.
    fn pkg(v: serde_json::Value) -> sandogasa_fedrq::PkgInfo {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_batched_answer_is_matched_back_to_each_capability() {
        // One `-P` query for three capabilities returns the providers
        // of all of them together; each capability resolves only when
        // one of them provides it.
        let providers = [
            pkg(serde_json::json!({"name": "libical",
                "provides": ["libical.so.3()(64bit)", "libical = 3.0.20-11.fc46"]})),
            pkg(serde_json::json!({"name": "xapian-core-libs",
                "provides": ["libxapian.so.30()(64bit)"]})),
        ];
        let caps: Vec<String> = [
            "libical.so.3()(64bit)",
            "libxapian.so.30()(64bit)",
            "nonexistent-cap",
            "libical",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            resolved_by(&caps, &providers)
                .into_iter()
                .collect::<Vec<_>>(),
            [
                "libical",
                "libical.so.3()(64bit)",
                "libxapian.so.30()(64bit)"
            ]
        );
    }

    #[test]
    fn a_file_require_is_found_under_its_merged_usr_path() {
        // As f46 answers: util-linux ships /usr/bin/nologin and
        // declares /sbin/nologin, postfix ships /usr/bin/sendmail and
        // declares /usr/sbin/sendmail.
        let providers = [
            pkg(serde_json::json!({"name": "bash", "files": ["/usr/bin/bash", "/usr/bin/sh"]})),
            pkg(serde_json::json!({"name": "util-linux", "files": ["/usr/bin/nologin"]})),
            pkg(
                serde_json::json!({"name": "postfix", "files": ["/usr/bin/sendmail"],
                "provides": ["/usr/sbin/sendmail"]}),
            ),
        ];
        let paths: Vec<String> = [
            "/bin/sh",
            "/sbin/nologin",
            "/usr/sbin/sendmail",
            "/usr/bin/missing",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            paths_shipped(&paths, &providers)
                .into_iter()
                .collect::<Vec<_>>(),
            ["/bin/sh", "/sbin/nologin", "/usr/sbin/sendmail"]
        );
    }

    #[test]
    fn chunks_cover_every_item_once() {
        let items: Vec<String> = (0..QUERY_CHUNK * 2 + 7).map(|i| i.to_string()).collect();
        let mut seen = chunked(&items, "test", true, |chunk| chunk.to_vec());
        seen.sort_by_key(|s| s.parse::<usize>().unwrap());
        assert_eq!(seen, items);
    }
}
