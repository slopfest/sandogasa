// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OSV (<https://osv.dev>) as a source of fixes.
//!
//! OSV aggregates upstream advisory databases — the PSF's for CPython,
//! PyPI's, Go's, RustSec's — where NVD often records no fixed version
//! at all. A record states a fix two ways, and they are kept apart here
//! because they are not equally safe to act on:
//!
//! - an **ecosystem range** (PyPI, crates.io, Go, …) names the fixed
//!   version outright, as GitHub's advisory database does, so it is
//!   returned in the same shape ([`GhsaVuln`]) and judged the same way;
//! - a **git range** names fix commits in a repository. The affected
//!   *tag lists* OSV derives from those are not used for versions: they
//!   leave out tags off a branch's line (CPython's v3.13.9, an
//!   expedited release, is missing from every list, though 3.13.8 and
//!   3.13.10 are affected), so "the first release not listed" can name
//!   an affected release as the fix. Commits are returned as they are,
//!   for a check of whether a given tag contains one.
//!
//! Distribution ecosystems (Debian, Alpine, SUSE, AlmaLinux, …) are
//! ignored: their versions are their packages', not upstream's.

use crate::advisory::GhsaVuln;

/// What one or more OSV records say about a CVE.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OsvFix {
    /// The record the ecosystem ranges came from (`PYSEC-2026-12`),
    /// or the CVE's own when they all did.
    pub id: String,
    /// Upstream-ecosystem ranges, in GitHub's ecosystem names so they
    /// are judged like a GHSA record's.
    pub vulns: Vec<GhsaVuln>,
    /// Fix commits per repository, one per branch a fix landed on.
    pub fixes: Vec<FixCommits>,
}

/// The commits that fix a CVE in one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FixCommits {
    /// The repository URL as OSV gives it
    /// (`https://github.com/python/cpython`).
    pub repo: String,
    /// Full commit hashes, in OSV's order.
    pub commits: Vec<String>,
}

/// OSV's API URL for a vulnerability by id (a CVE or an alias).
pub fn vuln_url(id: &str) -> String {
    format!("https://api.osv.dev/v1/vulns/{id}")
}

/// GitHub's ecosystem name for an upstream OSV ecosystem, or `None`
/// for a distribution's (whose versions are its packages') or one
/// nothing here judges.
pub fn github_ecosystem(osv: &str) -> Option<&'static str> {
    Some(match osv {
        "PyPI" => "pip",
        "crates.io" => "rust",
        "npm" => "npm",
        "Go" => "go",
        "Maven" => "maven",
        "RubyGems" => "rubygems",
        "NuGet" => "nuget",
        "Packagist" => "composer",
        "Hex" => "erlang",
        "Pub" => "pub",
        _ => return None,
    })
}

/// The aliases of a record worth reading for ecosystem ranges: the
/// upstream databases OSV hosts. GHSA records are left out, since
/// GitHub's own database is asked for them already.
pub fn upstream_aliases(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v["aliases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .filter(|a| {
            ["PYSEC-", "GO-", "RUSTSEC-", "PSF-"]
                .iter()
                .any(|p| a.starts_with(p))
        })
        .map(str::to_string)
        .collect()
}

/// Read one OSV record: its upstream-ecosystem ranges and its fix
/// commits. `None` when the text is not an OSV record.
pub fn parse(json: &str) -> Option<OsvFix> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let id = v["id"].as_str()?.to_string();
    let mut out = OsvFix {
        id,
        ..Default::default()
    };
    for affected in v["affected"].as_array().into_iter().flatten() {
        let package = &affected["package"];
        for range in affected["ranges"].as_array().into_iter().flatten() {
            let events: Vec<(&str, &str)> = range["events"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e.as_object())
                .flat_map(|e| {
                    e.iter()
                        .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)))
                })
                .collect();
            match range["type"].as_str() {
                Some("GIT") => {
                    let Some(repo) = range["repo"].as_str() else {
                        continue;
                    };
                    let commits: Vec<String> = events
                        .iter()
                        .filter(|(k, _)| *k == "fixed")
                        .map(|(_, c)| c.to_string())
                        .collect();
                    if commits.is_empty() {
                        continue;
                    }
                    match out.fixes.iter_mut().find(|f| f.repo == repo) {
                        Some(f) => f.commits.extend(commits),
                        None => out.fixes.push(FixCommits {
                            repo: repo.to_string(),
                            commits,
                        }),
                    }
                }
                Some("ECOSYSTEM" | "SEMVER") => {
                    let (Some(ecosystem), Some(name)) = (
                        package["ecosystem"].as_str().and_then(github_ecosystem),
                        package["name"].as_str(),
                    ) else {
                        continue;
                    };
                    out.vulns
                        .extend(
                            intervals(&events)
                                .into_iter()
                                .map(|(range, patched)| GhsaVuln {
                                    ecosystem: ecosystem.to_string(),
                                    package: name.to_string(),
                                    range: Some(range),
                                    patched,
                                }),
                        );
                }
                _ => {}
            }
        }
    }
    Some(out)
}

/// Fold another record into this one: its ranges and commits are
/// added, and the id names the record the ranges came from.
pub fn merge(into: &mut OsvFix, other: OsvFix) {
    if into.vulns.is_empty() && !other.vulns.is_empty() {
        into.id = other.id;
    }
    for v in other.vulns {
        if !into.vulns.contains(&v) {
            into.vulns.push(v);
        }
    }
    for f in other.fixes {
        match into.fixes.iter_mut().find(|g| g.repo == f.repo) {
            Some(g) => {
                for c in f.commits {
                    if !g.commits.contains(&c) {
                        g.commits.push(c);
                    }
                }
            }
            None => into.fixes.push(f),
        }
    }
}

/// `(owner, repo)` for a GitHub repository URL
/// (`https://github.com/python/cpython`, with or without `.git` or a
/// trailing slash); `None` for any other host.
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut parts = rest.trim_end_matches('/').splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next()?.trim_end_matches(".git");
    (!repo.is_empty() && parts.next().is_none()).then(|| (owner.to_string(), repo.to_string()))
}

/// The release tags on `version`'s line, oldest first, as
/// `(patch, tag)`: for 3.13.9 every `v3.13.N` or `3.13.N`. `None` when
/// the version is not `major.minor.patch` or has no tag of its own, so
/// there is nothing to compare against. The position of its own tag is
/// returned with the list.
pub fn line_tags(tags: &[String], version: &str) -> Option<(usize, Vec<(u64, String)>)> {
    let mut parts = version.split('.');
    let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if parts.next().is_some() || ![major, minor, patch].iter().all(|p| numeric(p)) {
        return None;
    }
    let prefix = format!("{major}.{minor}.");
    let mut line: Vec<(u64, String)> = tags
        .iter()
        .filter_map(|t| {
            let n = t.strip_prefix('v').unwrap_or(t).strip_prefix(&prefix)?;
            n.parse()
                .ok()
                .filter(|_| numeric(n))
                .map(|n| (n, t.clone()))
        })
        .collect();
    line.sort();
    line.dedup_by_key(|(n, _)| *n);
    let own: u64 = patch.parse().ok()?;
    let at = line.iter().position(|(n, _)| *n == own)?;
    Some((at, line))
}

/// The first tag after `after` on a release line that contains a fix,
/// asking `contains(tag)` as few times as it can: the newest tag first
/// (none of the line has the fix if that one lacks it), then halving.
/// `Some(Some(i))` names `line[i]`; `Some(None)` means no release on
/// the line has the fix yet; `None` that a question went unanswered.
pub async fn first_fixed<F, Fut>(
    line: &[(u64, String)],
    after: usize,
    mut contains: F,
) -> Option<Option<usize>>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Option<bool>>,
{
    let newest = line.len().checked_sub(1)?;
    if newest <= after || !contains(line[newest].1.clone()).await? {
        return Some(None);
    }
    let (mut lo, mut hi) = (after + 1, newest);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if contains(line[mid].1.clone()).await? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Some(Some(lo))
}

/// An ecosystem range's events as GHSA-style ranges, one per
/// interval: `introduced 0, fixed 2.0` is `< 2.0`, patched in 2.0;
/// `introduced 1.0, last_affected 1.4` is `>= 1.0, <= 1.4`, with no
/// fix.
fn intervals(events: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut start: Option<&str> = None;
    for (kind, version) in events {
        match *kind {
            "introduced" => start = Some(version),
            "fixed" | "last_affected" => {
                let lower = start
                    .filter(|s| *s != "0")
                    .map(|s| format!(">= {s}, "))
                    .unwrap_or_default();
                if *kind == "fixed" {
                    out.push((format!("{lower}< {version}"), Some(version.to_string())));
                } else {
                    out.push((format!("{lower}<= {version}"), None));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CVE-2025-8194 as OSV records it (2026-11-05), trimmed: CPython's
    /// fix commits, the tag list derived from them, and a distro's
    /// record that must not count.
    const CPYTHON: &str = r#"{
      "id": "CVE-2025-8194",
      "aliases": ["BIT-python-2025-8194", "PSF-2025-11", "GHSA-xxxx-yyyy-zzzz"],
      "affected": [
        {"ranges": [{"type": "GIT", "repo": "https://github.com/python/cpython",
          "events": [{"introduced": "0"},
                     {"fixed": "cdae923ffe187d6ef916c0f665a31249619193fe"},
                     {"fixed": "7040aa54f14676938970e10c5f74ea93cd56aa38"}]}],
         "versions": ["v3.13.4", "v3.13.5"]},
        {"package": {"ecosystem": "Debian:12", "name": "python3.11"},
         "ranges": [{"type": "ECOSYSTEM",
           "events": [{"introduced": "0"}, {"fixed": "3.11.2-6+deb12u7"}]}]}
      ]
    }"#;

    /// A PyPI record with two series fixed and one left open.
    const PYPI: &str = r#"{
      "id": "PYSEC-2026-12",
      "affected": [
        {"package": {"ecosystem": "PyPI", "name": "requests"},
         "ranges": [{"type": "ECOSYSTEM", "events": [
           {"introduced": "0"}, {"fixed": "2.32.4"},
           {"introduced": "3.0.0"}, {"fixed": "3.0.2"},
           {"introduced": "4.0.0a1"}, {"last_affected": "4.0.0a3"}]}]}
      ]
    }"#;

    #[test]
    fn git_ranges_give_commits_and_never_versions() {
        let fix = parse(CPYTHON).unwrap();
        assert_eq!(fix.id, "CVE-2025-8194");
        assert!(fix.vulns.is_empty(), "{:?}", fix.vulns);
        assert_eq!(
            fix.fixes,
            [FixCommits {
                repo: "https://github.com/python/cpython".to_string(),
                commits: vec![
                    "cdae923ffe187d6ef916c0f665a31249619193fe".to_string(),
                    "7040aa54f14676938970e10c5f74ea93cd56aa38".to_string(),
                ],
            }]
        );
        assert_eq!(upstream_aliases(CPYTHON), ["PSF-2025-11"]);
    }

    #[test]
    fn ecosystem_ranges_read_like_a_ghsa_record() {
        let fix = parse(PYPI).unwrap();
        let ranges: Vec<_> = fix
            .vulns
            .iter()
            .map(|v| {
                (
                    v.ecosystem.as_str(),
                    v.range.as_deref().unwrap(),
                    v.patched.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            ranges,
            [
                ("pip", "< 2.32.4", Some("2.32.4")),
                ("pip", ">= 3.0.0, < 3.0.2", Some("3.0.2")),
                ("pip", ">= 4.0.0a1, <= 4.0.0a3", None),
            ]
        );
        assert!(fix.vulns.iter().all(|v| v.package == "requests"));
    }

    #[test]
    fn github_repositories_are_read_from_their_urls() {
        let cpython = Some(("python".to_string(), "cpython".to_string()));
        assert_eq!(github_repo("https://github.com/python/cpython"), cpython);
        assert_eq!(
            github_repo("https://github.com/python/cpython.git"),
            cpython
        );
        assert_eq!(github_repo("https://github.com/python/cpython/"), cpython);
        assert_eq!(github_repo("https://gitlab.com/python/cpython"), None);
        assert_eq!(
            github_repo("https://github.com/python/cpython/commit/abc"),
            None
        );
    }

    #[test]
    fn a_version_s_line_is_its_own_tags_in_release_order() {
        let tags: Vec<String> = [
            "v3.13.10",
            "v3.13.9",
            "v3.13.8",
            "v3.14.0",
            "v3.13.0rc1",
            "v3.12.9",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (at, line) = line_tags(&tags, "3.13.9").unwrap();
        assert_eq!(
            line.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            [8, 9, 10],
            "release candidates and other lines are left out"
        );
        assert_eq!(line[at].1, "v3.13.9");
        // No tag of its own, or not a three-part version: nothing to do.
        assert_eq!(line_tags(&tags, "3.13.7"), None);
        assert_eq!(line_tags(&tags, "3.13"), None);
        assert_eq!(line_tags(&tags, "3.13.9a"), None);
        // Tags without the v work the same.
        let plain = vec!["1.2.3".to_string(), "1.2.4".to_string()];
        assert_eq!(line_tags(&plain, "1.2.3").unwrap().0, 0);
    }

    /// v3.13.8 to v3.13.18, as `line_tags` gives them.
    fn line_313() -> Vec<(u64, String)> {
        (8..=18).map(|n| (n, format!("v3.13.{n}"))).collect()
    }

    #[test]
    fn the_first_fixed_release_is_found_by_halving() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let line = line_313();
        // Fixed from 3.13.14 on; the copy is 3.13.9 (index 1).
        let mut asked = Vec::new();
        let found = rt.block_on(first_fixed(&line, 1, |tag: String| {
            asked.push(tag.clone());
            let n: u64 = tag.rsplit('.').next().unwrap().parse().unwrap();
            async move { Some(n >= 14) }
        }));
        assert_eq!(found.map(|i| i.map(|i| line[i].0)), Some(Some(14)));
        assert!(asked.len() <= 5, "{asked:?}");
        // No release on the line has it: one question, about the newest.
        let mut asked = 0;
        let none = rt.block_on(first_fixed(&line, 1, |_| {
            asked += 1;
            async { Some(false) }
        }));
        assert_eq!((none, asked), (Some(None), 1));
        // An unanswered question leaves it open rather than guessing.
        assert_eq!(rt.block_on(first_fixed(&line, 1, |_| async { None })), None);
        // The copy is the newest release: nothing after it to find.
        assert_eq!(
            rt.block_on(first_fixed(&line, 10, |_| async { Some(true) })),
            Some(None)
        );
    }

    #[test]
    fn a_merged_alias_lends_its_id_only_with_its_ranges() {
        let mut cve = parse(CPYTHON).unwrap();
        merge(&mut cve, parse(PYPI).unwrap());
        assert_eq!(cve.id, "PYSEC-2026-12");
        assert_eq!(cve.vulns.len(), 3);
        assert_eq!(cve.fixes[0].commits.len(), 2);
        // A record with commits only keeps the id the ranges came from.
        let mut again = cve.clone();
        merge(&mut again, parse(CPYTHON).unwrap());
        assert_eq!(again, cve);
        assert_eq!(parse("not json"), None);
    }
}
