// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Judging a bundled copy of a library against a CVE's fix.
//!
//! A package that bundles a library declares it as
//! `bundled(<library>) = <version>`, and a CVE in the library reaches
//! the package through that copy. Whether a build carries the fix is a
//! question about the copy, not the package's own version: an
//! asahi-installer 0.9.2 build bundling Python 3.13.17 is fixed for a
//! CVE fixed in 3.13.17, whatever asahi-installer's number says.
//!
//! The pieces, shared by every tool that asks:
//!
//! - [`bundled_version`] reads a declaration from a provides list;
//! - [`Copies::declared`] reads it from a build in Koji, which knows a
//!   build before any repository does;
//! - [`against_fixes`] judges a copy against the fixed versions NVD or
//!   an advisory records for the library;
//! - [`Copies::git_verdict`] judges it by git when no source gives a
//!   version on the copy's release line: whether the copy's own release
//!   tag holds one of the fix commits OSV records.
//!
//! The answers that never change, what a finished build provides and
//! whether a tag contains a commit, are kept under
//! `$XDG_CACHE_HOME/sandogasa-cve/`, shared by every tool; a repository's
//! tag list is kept there for a day.

use std::time::Duration;

use sandogasa_cli::cache::{DiskCache, url_key};

use crate::advisory;
use crate::facts::in_vulnerable_range;
use crate::osv::{self, OsvFix};
use crate::version::version_gte;

/// What git says about a bundled copy and a CVE's fix commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitVerdict {
    /// The copy's own release tag holds a fix commit.
    pub fixed: bool,
    /// The first release on the copy's line with the fix, and its tag:
    /// the copy's own when `fixed`, `None` when the line has none yet.
    pub first_fixed: Option<(String, String)>,
    /// What to say about it, after "at or past the fix in" or "older
    /// than the fix in".
    pub what: String,
}

/// Where bundled copies are read and judged, with the caches the
/// answers are kept in.
pub struct Copies {
    http: reqwest::Client,
    github_token: Option<String>,
    verbose: bool,
    /// The Koji hub what a build provides is read from, in batches;
    /// `None` reads it through the `koji` CLI, one RPM at a time.
    hub: Option<String>,
    provides: DiskCache,
    contains: DiskCache,
    tags: DiskCache,
}

/// How long a repository's tag list is kept: new releases are tagged,
/// so it goes stale, unlike whether a tag contains a commit.
const TAGS_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The cache directory the shared answers live under.
const CACHE_TOOL: &str = "sandogasa-cve";

/// How many RPMs' provides one `multiCall` asks for: a build with
/// hundreds of subpackages is read in a few requests, none of them
/// large enough to trouble the hub.
const PROVIDES_CHUNK: usize = 200;

/// Everything the build `nvr` provides, read from the Koji hub at
/// `hub_url` in the format [`sandogasa_koji::build_provides`] writes:
/// the build's RPMs in one request, then one RPM per subpackage in
/// batches of [`PROVIDES_CHUNK`]. An RPM whose provides cannot be read
/// fails the whole read, as there.
///
/// The hub client blocks, and blocking inside an async runtime panics,
/// so the requests run on a thread of their own.
fn provides_from_hub(hub_url: &str, nvr: &str) -> Result<String, String> {
    std::thread::scope(|s| {
        s.spawn(|| {
            let hub = sandogasa_kojihub::HubClient::new(hub_url);
            let nvrs = [nvr.to_string()];
            let nvras = sandogasa_kojihub::retry(2, || hub.builds_rpm_nvras(&nvrs))
                .map_err(|e| e.to_string())?
                .pop()
                .ok_or("the hub answered no build")?
                .map_err(|e| e.to_string())?;
            // A build with no binary RPMs provides nothing, as the
            // koji CLI's reading of it says too.
            let wanted: Vec<String> = sandogasa_koji::subpackage_nvras(&nvras)
                .into_iter()
                .map(str::to_string)
                .collect();
            let mut text = String::new();
            for chunk in wanted.chunks(PROVIDES_CHUNK) {
                let answers = sandogasa_kojihub::retry(2, || hub.rpms_deps(chunk, 1))
                    .map_err(|e| e.to_string())?;
                for (nvra, answer) in chunk.iter().zip(answers) {
                    let deps = answer.map_err(|e| format!("{nvra}: {e}"))?;
                    sandogasa_koji::push_provides(&mut text, &deps);
                }
            }
            Ok(text)
        })
        .join()
        .unwrap_or_else(|_| Err("reading from the hub panicked".to_string()))
    })
}

impl Copies {
    /// Copies read through `http`, with `github_token` (when given) to
    /// lift GitHub's rate limit, and the shared caches; `refresh`
    /// ignores what they hold, as a tool's `--refresh` does.
    pub fn new(
        http: reqwest::Client,
        github_token: Option<String>,
        refresh: bool,
        verbose: bool,
    ) -> Self {
        Copies {
            http,
            github_token,
            verbose,
            hub: sandogasa_kojihub::config::hub_url("koji"),
            provides: DiskCache::new(CACHE_TOOL, "build-provides", None, refresh),
            contains: DiskCache::new(CACHE_TOOL, "git-contains", None, refresh),
            tags: DiskCache::new(CACHE_TOOL, "github-tags", Some(TAGS_TTL), refresh),
        }
    }

    /// The same, with every cache under `dir`: for tests, which must
    /// not read or write the user's.
    pub fn at(dir: &std::path::Path, http: reqwest::Client, github_token: Option<String>) -> Self {
        let cache = |what: &str, ttl| DiskCache::at(Some(dir.join(what)), false, ttl);
        Copies {
            http,
            github_token,
            verbose: false,
            hub: None,
            provides: cache("build-provides", None),
            contains: cache("git-contains", None),
            tags: cache("github-tags", Some(TAGS_TTL)),
        }
    }

    /// Read what a build provides from the Koji hub at `hub_url`
    /// rather than the one the `koji` CLI's configuration names.
    pub fn with_hub(mut self, hub_url: &str) -> Self {
        self.hub = Some(hub_url.to_string());
        self
    }

    /// Everything the build `nvr` provides, from Koji, one per line
    /// (see [`sandogasa_koji::build_provides`]). Read from the hub in
    /// batches when one is configured, through the `koji` CLI one RPM
    /// at a time when not, or when the hub fails. `None` when neither
    /// can read the build; a list read in part is never kept.
    pub fn build_provides(&self, nvr: &str) -> Option<String> {
        let key = format!("{nvr}.provides");
        if let Some(text) = self.provides.load(&key) {
            return Some(text);
        }
        if self.hub.is_none() && !sandogasa_koji::is_available() {
            return None;
        }
        if self.verbose {
            eprintln!("Reading what {nvr} provides from Koji...");
        }
        if let Some(hub) = &self.hub {
            match provides_from_hub(hub, nvr) {
                Ok(text) => {
                    self.provides.store(&key, &text);
                    return Some(text);
                }
                Err(e) => {
                    // Said whatever the verbosity: the CLI reads one
                    // RPM at a time, which takes minutes on a big build.
                    eprintln!(
                        "warning: the Koji hub could not say what {nvr} provides ({e}); \
                         asking the koji CLI, one RPM at a time"
                    );
                }
            }
        }
        if !sandogasa_koji::is_available() {
            return None;
        }
        match sandogasa_koji::build_provides(nvr, None) {
            Ok(text) => {
                self.provides.store(&key, &text);
                Some(text)
            }
            Err(e) => {
                eprintln!("warning: what {nvr} provides could not be read from Koji: {e}");
                None
            }
        }
    }

    /// The version of `library` the build `nvr` declares it bundles,
    /// when it declares one with a version.
    pub fn declared(&self, nvr: &str, library: &str) -> Option<String> {
        bundled_version(&self.build_provides(nvr)?, library).flatten()
    }

    /// A GitHub repository's tag names, kept a day.
    pub async fn github_tags(&self, owner: &str, repo: &str) -> Option<Vec<String>> {
        let key = url_key(&format!("https://api.github.com/repos/{owner}/{repo}/tags"));
        if let Some(text) = self.tags.load(&key) {
            return Some(text.lines().map(str::to_string).collect());
        }
        if self.verbose {
            eprintln!("Listing {owner}/{repo}'s tags on GitHub...");
        }
        let mut tags = Vec::new();
        // CPython has some 600 tags; a repository with far more is not
        // worth paging through for this.
        for page in 1..=20 {
            let url = format!(
                "https://api.github.com/repos/{owner}/{repo}/tags?per_page=100&page={page}"
            );
            let json =
                advisory::fetch_github(&self.http, &url, self.github_token.as_deref()).await?;
            let names: Vec<String> = serde_json::from_str::<serde_json::Value>(&json)
                .ok()?
                .as_array()?
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect();
            if names.is_empty() {
                break;
            }
            tags.extend(names);
        }
        self.tags.store(&key, &tags.join("\n"));
        Some(tags)
    }

    /// Whether `tag` contains commit `sha`, asked of GitHub's compare
    /// API and kept for good: a tag does not move. `None` when GitHub
    /// does not answer.
    pub async fn tag_contains(
        &self,
        owner: &str,
        repo: &str,
        tag: &str,
        sha: &str,
    ) -> Option<bool> {
        let key = url_key(&format!("{owner}/{repo}/{tag}/{sha}"));
        if let Some(answer) = self.contains.load(&key) {
            return Some(answer == "true");
        }
        let url = format!("https://api.github.com/repos/{owner}/{repo}/compare/{tag}...{sha}");
        let json = advisory::fetch_github(&self.http, &url, self.github_token.as_deref()).await?;
        let status = serde_json::from_str::<serde_json::Value>(&json).ok()?["status"]
            .as_str()?
            .to_string();
        let contained = match status.as_str() {
            "behind" | "identical" => true,
            "ahead" | "diverged" => false,
            _ => return None,
        };
        self.contains
            .store(&key, if contained { "true" } else { "false" });
        Some(contained)
    }

    /// Whether a bundled copy at `version` carries `cve_id`'s fix, from
    /// the fix commits `fix` (OSV's record) lists and the copy's own
    /// release tag.
    ///
    /// `fixed` when a fix commit is in its tag; otherwise the first
    /// release on its line that has the fix, or a note that the line
    /// has none yet. `None` when it cannot be told: no fix commits, no
    /// GitHub repository, no tag for the version, or GitHub not
    /// answering. Versions OSV derives from the commits are not used:
    /// its tag lists skip tags off a branch's line.
    pub async fn git_verdict(
        &self,
        fix: &OsvFix,
        cve_id: &str,
        version: &str,
    ) -> Option<GitVerdict> {
        for commits in &fix.fixes {
            let Some((owner, repo)) = osv::github_repo(&commits.repo) else {
                continue;
            };
            let tags = self.github_tags(&owner, &repo).await?;
            let Some((at, line)) = osv::line_tags(&tags, version) else {
                continue;
            };
            let own = line[at].1.clone();
            // Said only when GitHub is actually asked: a repeat run, or
            // a second bug on the same CVE, is answered from the cache.
            let first_known = commits.commits.first().is_some_and(|sha| {
                self.contains
                    .load(&url_key(&format!("{owner}/{repo}/{own}/{sha}")))
                    .is_some()
            });
            if self.verbose && !first_known {
                eprintln!(
                    "Asking GitHub whether {own} has {cve_id}'s fix ({} commit(s))...",
                    commits.commits.len()
                );
            }
            for sha in &commits.commits {
                if self.tag_contains(&owner, &repo, &own, sha).await? {
                    return Some(GitVerdict {
                        fixed: true,
                        first_fixed: Some((version.to_string(), own.clone())),
                        what: format!("commit {} (in {own})", &sha[..sha.len().min(12)]),
                    });
                }
            }
            // Not in this copy: which commit is this line's fix, and the
            // first release with it, found by halving the line's later tags.
            let series = version.rsplit_once('.').map_or(version, |(s, _)| s);
            let newest = line.last()?.1.clone();
            let mut line_fix = None;
            for sha in &commits.commits {
                if self.tag_contains(&owner, &repo, &newest, sha).await? {
                    line_fix = Some(sha.clone());
                    break;
                }
            }
            let none_yet = GitVerdict {
                fixed: false,
                first_fixed: None,
                what: format!("a later series, not yet in any {series} release (newest {newest})"),
            };
            let Some(sha) = line_fix else {
                return Some(none_yet);
            };
            let first = osv::first_fixed(&line, at, |tag| {
                let (owner, repo, sha) = (owner.clone(), repo.clone(), sha.clone());
                async move { self.tag_contains(&owner, &repo, &tag, &sha).await }
            })
            .await?;
            return Some(match first {
                Some(i) => {
                    let release = format!("{series}.{}", line[i].0);
                    GitVerdict {
                        fixed: false,
                        first_fixed: Some((release.clone(), line[i].1.clone())),
                        what: release,
                    }
                }
                None => none_yet,
            });
        }
        None
    }
}

/// The version a `bundled(<library>…) = <version>` declaration names,
/// searched in `text` (a provides list; spec `Provides:` lines are
/// tolerated too): `None` when nothing is declared, `Some(None)` when
/// it is declared without a version. The prefix allows for a versioned
/// name such as `bundled(gstreamer1)`; `bundled(crate(gstreamer))` does
/// not match, a binding crate named after the library being no copy of
/// it.
pub fn bundled_version(text: &str, library: &str) -> Option<Option<String>> {
    let needle = format!("bundled({}", library.to_ascii_lowercase());
    // The name has to end where the needle does, a version suffix
    // aside: `bundled(gstreamer1)` is a copy of GStreamer, while
    // `bundled(golang(github.com/…))` is a vendored module and not a
    // copy of the toolchain, though `bundled(go` prefixes it. Letters
    // glued on are another word — the same rule package names follow.
    let named = |l: &str| {
        l.find(&needle).is_some_and(|at| {
            let rest = &l[at + needle.len()..];
            let digits = rest
                .find(|c: char| !c.is_ascii_digit() && c != '.')
                .unwrap_or(rest.len());
            rest[digits..].starts_with(')')
        })
    };
    let line = text.lines().find(|l| {
        let l = l.trim().to_ascii_lowercase();
        named(&l) && !l.starts_with('#') && (!l.contains(':') || l.starts_with("provides:"))
    })?;
    let after = &line[line.to_ascii_lowercase().find(&needle)? + needle.len()..];
    let after = after.split_once(')')?.1.trim_start();
    Some(
        after
            .strip_prefix('=')
            .map(|v| v.split_whitespace().next().unwrap_or_default().to_string())
            .filter(|v| !v.is_empty()),
    )
}

/// Every library `provides` declares a versioned bundled copy of, as
/// `(library, version)`, lowercased. Vendored crates and Go modules
/// (`bundled(crate(…))`, `bundled(golang(…))`) are left out: they are
/// named by their ecosystem, not by a library a CVE tracker names.
pub fn declarations(provides: &str) -> Vec<(String, String)> {
    provides
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("bundled(")?;
            let (name, after) = rest.split_once(')')?;
            if name.contains('(') || name.is_empty() {
                return None;
            }
            let version = after
                .trim_start()
                .strip_prefix('=')?
                .split_whitespace()
                .next()?;
            Some((name.to_ascii_lowercase(), version.to_string()))
        })
        .collect()
}

/// A project's name as Fedora packages it: CPython is `python`.
pub fn fedora_spelling(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "cpython" => "python".to_string(),
        _ => lower,
    }
}

/// The bundled copy a CVE is about, among those `provides` declares:
/// one whose library the tracker's `summary` names as a word
/// (`asahi-installer: python: …`, `CPython: …`), or one of the
/// `products` NVD says the CVE affects. `None` when no declared copy is
/// named, which is most packages and most CVEs: the CVE is then the
/// package's own.
pub fn copy_for_cve(
    provides: &str,
    summary: &str,
    products: &[String],
) -> Option<(String, String)> {
    let mut named: Vec<String> = summary
        .split(|c: char| !(c.is_ascii_alphanumeric() || "-_.+".contains(c)))
        .map(|w| fedora_spelling(w.trim_end_matches(['.', '-'])))
        .filter(|w| !w.is_empty())
        .collect();
    named.extend(products.iter().map(|p| fedora_spelling(p)));
    declarations(provides)
        .into_iter()
        .find(|(library, _)| named.iter().any(|n| n == library))
}

/// Whether a copy of `library` at `version` carries the fix, judged by
/// the fixed versions and vulnerable ranges recorded for it (entries
/// for other products are ignored): at or past a fixed version, and
/// out of every range still marked vulnerable. `None` when nothing is
/// recorded for the library. The second field names the fix that
/// matters for this copy: the highest it has reached, or the lowest
/// still ahead of it, not every series that was patched.
pub fn against_fixes(
    version: &str,
    library: &str,
    fixed: &[sandogasa_nvd::FixedVersion],
    ranges: &[sandogasa_nvd::VulnerableRange],
) -> Option<(bool, String)> {
    let fixed: Vec<_> = fixed
        .iter()
        .filter(|f| f.product.eq_ignore_ascii_case(library))
        .collect();
    if fixed.is_empty() {
        return None;
    }
    let has_fix = fixed.iter().any(|f| version_gte(version, &f.version))
        && !ranges
            .iter()
            .filter(|r| r.product.eq_ignore_ascii_case(library))
            .any(|r| in_vulnerable_range(r, version));
    let mut versions: Vec<&str> = fixed.iter().map(|f| f.version.as_str()).collect();
    versions.sort_by(|a, b| {
        if a == b {
            std::cmp::Ordering::Equal
        } else if version_gte(a, b) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        }
    });
    let nearest = if has_fix {
        versions.iter().rev().find(|f| version_gte(version, f))
    } else {
        versions.iter().find(|f| !version_gte(version, f))
    };
    Some((
        has_fix,
        nearest.map_or_else(|| versions.join("/"), |f| f.to_string()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(product: &str, version: &str) -> sandogasa_nvd::FixedVersion {
        sandogasa_nvd::FixedVersion {
            product: product.into(),
            version: version.into(),
        }
    }

    /// An XML-RPC `multiCall` answer of `results`, each a `<value>`.
    fn multicall_xml(results: &[String]) -> String {
        let wrapped: String = results
            .iter()
            .map(|r| format!("<value><array><data>{r}</data></array></value>"))
            .collect();
        format!(
            "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data>\
             {wrapped}</data></array></value></param></params></methodResponse>"
        )
    }

    fn rpm_xml(name: &str, arch: &str) -> String {
        format!(
            "<value><struct>\
             <member><name>name</name><value><string>{name}</string></value></member>\
             <member><name>version</name><value><string>24.0.0</string></value></member>\
             <member><name>release</name><value><string>3.fc46</string></value></member>\
             <member><name>arch</name><value><string>{arch}</string></value></member>\
             </struct></value>"
        )
    }

    fn provide_xml(name: &str, version: &str) -> String {
        format!(
            "<value><struct>\
             <member><name>name</name><value><string>{name}</string></value></member>\
             <member><name>version</name><value><string>{version}</string></value></member>\
             <member><name>type</name><value><int>1</int></value></member>\
             </struct></value>"
        )
    }

    /// The hub is asked for the build's RPMs once and for one RPM per
    /// subpackage in one batch, from inside an async runtime (where a
    /// blocking client would panic). Read through `provides_from_hub`,
    /// which has no CLI to fall back to, so a request the mocks do not
    /// expect fails the test rather than reaching a real Koji.
    #[tokio::test]
    async fn a_builds_provides_are_read_from_the_hub_in_one_batch() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};

        let server = MockServer::start().await;
        let rpms = [
            rpm_xml("asterisk", "src"),
            rpm_xml("asterisk", "x86_64"),
            rpm_xml("asterisk", "i686"),
            rpm_xml("asterisk-calendar", "x86_64"),
            rpm_xml("asterisk-debuginfo", "x86_64"),
        ]
        .concat();
        Mock::given(method("POST"))
            .and(body_string_contains("listBuildRPMs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(multicall_xml(&[format!(
                    "<value><array><data>{rpms}</data></array></value>"
                )])),
            )
            .expect(1)
            .mount(&server)
            .await;
        let main = format!(
            "<value><array><data>{}{}</data></array></value>",
            provide_xml("asterisk", "24.0.0-3.fc46"),
            provide_xml("bundled(pjproject)", "2.15.1"),
        );
        let calendar = format!(
            "<value><array><data>{}</data></array></value>",
            provide_xml("config(asterisk-calendar)", ""),
        );
        Mock::given(method("POST"))
            .and(body_string_contains("getRPMDeps"))
            // One per subpackage: asterisk once, not for i686 too, and
            // no debuginfo.
            .and(body_string_contains("asterisk-24.0.0-3.fc46.x86_64"))
            .and(body_string_contains(
                "asterisk-calendar-24.0.0-3.fc46.x86_64",
            ))
            .and(|req: &Request| {
                let body = String::from_utf8_lossy(&req.body);
                !body.contains(".i686") && !body.contains("-debuginfo")
            })
            .respond_with(
                ResponseTemplate::new(200).set_body_string(multicall_xml(&[main, calendar])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let text = provides_from_hub(&server.uri(), "asterisk-24.0.0-3.fc46").unwrap();
        assert_eq!(
            text,
            "asterisk = 24.0.0-3.fc46\n\
             bundled(pjproject) = 2.15.1\n\
             config(asterisk-calendar)\n"
        );
        assert_eq!(
            bundled_version(&text, "pjproject"),
            Some(Some("2.15.1".into()))
        );
    }

    /// A build read from the hub is kept, so the next question about
    /// it asks nothing, and its copies are read from what was kept.
    #[tokio::test]
    async fn what_the_hub_says_a_build_provides_is_kept() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        sandogasa_cli::install_crypto_provider();
        let server = MockServer::start().await;
        let rpms = format!(
            "<value><array><data>{}</data></array></value>",
            rpm_xml("asterisk", "x86_64")
        );
        Mock::given(method("POST"))
            .and(body_string_contains("listBuildRPMs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(multicall_xml(&[rpms])))
            .expect(1)
            .mount(&server)
            .await;
        let provides = format!(
            "<value><array><data>{}</data></array></value>",
            provide_xml("bundled(pjproject)", "2.15.1")
        );
        Mock::given(method("POST"))
            .and(body_string_contains("getRPMDeps"))
            .respond_with(ResponseTemplate::new(200).set_body_string(multicall_xml(&[provides])))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let copies = Copies::at(dir.path(), reqwest::Client::new(), None).with_hub(&server.uri());
        let nvr = "asterisk-24.0.0-3.fc46";
        assert_eq!(
            copies.build_provides(nvr).as_deref(),
            Some("bundled(pjproject) = 2.15.1\n")
        );
        // Answered from the cache: each mock expects one request.
        assert_eq!(copies.declared(nvr, "pjproject").as_deref(), Some("2.15.1"));
        assert_eq!(copies.declared(nvr, "openssl"), None);
    }

    /// A build with no binary RPMs provides nothing, from the hub as from
    /// the koji CLI, and no RPM is asked about.
    #[tokio::test]
    async fn a_build_without_binary_rpms_provides_nothing() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let only_src = format!(
            "<value><array><data>{}</data></array></value>",
            rpm_xml("asterisk", "src")
        );
        Mock::given(method("POST"))
            .and(body_string_contains("listBuildRPMs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(multicall_xml(&[only_src])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("getRPMDeps"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        assert_eq!(
            provides_from_hub(&server.uri(), "asterisk-24.0.0-3.fc46").as_deref(),
            Ok("")
        );
    }

    /// A fault for any subpackage fails the read, so a list missing the
    /// subpackage that declares a copy is never kept.
    #[test]
    fn a_subpackage_the_hub_cannot_read_fails_the_whole_read() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        let rpms = format!(
            "<value><array><data>{}{}</data></array></value>",
            rpm_xml("asterisk", "x86_64"),
            rpm_xml("asterisk-calendar", "x86_64"),
        );
        let fault = "<value><struct>\
             <member><name>faultCode</name><value><int>1000</int></value></member>\
             <member><name>faultString</name><value><string>No such rpm</string></value></member>\
             </struct></value>";
        rt.block_on(async {
            Mock::given(method("POST"))
                .and(body_string_contains("listBuildRPMs"))
                .respond_with(ResponseTemplate::new(200).set_body_string(multicall_xml(&[rpms])))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(body_string_contains("getRPMDeps"))
                .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                    "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data>\
                     <value><array><data><value><array><data></data></array></value></data></array></value>\
                     {fault}</data></array></value></param></params></methodResponse>"
                )))
                .mount(&server)
                .await;
        });
        let err = provides_from_hub(&server.uri(), "asterisk-24.0.0-3.fc46").unwrap_err();
        assert!(
            err.contains("asterisk-calendar-24.0.0-3.fc46.x86_64"),
            "{err}"
        );
        assert!(err.contains("No such rpm"), "{err}");
    }

    #[test]
    fn bundled_version_reads_the_declaration() {
        let provides = "asahi-installer-package = 0.9.2-4.fc46\n\
                        bundled(libffi) = 3.5.2\n\
                        bundled(python) = 3.13.17\n\
                        bundled(python-asn1) = 2.5.0\n";
        assert_eq!(
            bundled_version(provides, "python"),
            Some(Some("3.13.17".into()))
        );
        // A name that only starts the same is another library.
        assert_eq!(
            bundled_version(provides, "python-asn1"),
            Some(Some("2.5.0".into()))
        );
        assert_eq!(bundled_version(provides, "openssl"), None);
        assert_eq!(bundled_version("bundled(zlib)\n", "zlib"), Some(None));
        // A binding crate named for the library is not a copy of it.
        assert_eq!(
            bundled_version("bundled(crate(gstreamer)) = 0.23.0\n", "gstreamer"),
            None
        );
    }

    #[test]
    fn the_copy_a_cve_is_about_is_the_one_its_tracker_names() {
        let provides = "asahi-installer-package = 0.9.2-4.fc46\n\
                        bundled(libffi) = 3.5.2\n\
                        bundled(python) = 3.13.17\n\
                        bundled(python-asn1) = 2.5.0\n\
                        bundled(crate(serde)) = 1.0.0\n";
        assert_eq!(
            declarations(provides).len(),
            3,
            "crates are not libraries here"
        );
        let python = Some(("python".to_string(), "3.13.17".to_string()));
        assert_eq!(
            copy_for_cve(
                provides,
                "CVE-2026-12345 asahi-installer: python: Arbitrary file deletion",
                &[]
            ),
            python
        );
        assert_eq!(
            copy_for_cve(
                provides,
                "CVE-2026-2297 asahi-installer: CPython: Logging Bypass",
                &[]
            ),
            python
        );
        // NVD's product names it when the summary does not.
        assert_eq!(
            copy_for_cve(
                provides,
                "CVE-2025-15366 asahi-installer: IMAP command injection",
                &["cpython".to_string()]
            ),
            python
        );
        // A CVE that names no declared copy is the package's own.
        assert_eq!(
            copy_for_cve(provides, "CVE-2026-1 asahi-installer: installer flaw", &[]),
            None
        );
    }

    #[test]
    fn a_copy_is_judged_by_the_fix_on_its_own_line() {
        // NVD records one range per series: being past the 3.12 fix
        // says nothing about a 3.13 copy, which the 3.13 range covers.
        let fixes = [fixed("python", "3.12.12"), fixed("python", "3.13.14")];
        let range = |start: &str, end: &str| sandogasa_nvd::VulnerableRange {
            product: "python".into(),
            start_including: Some(start.into()),
            end_excluding: Some(end.into()),
            end_including: None,
        };
        let ranges = [range("3.12.0", "3.12.12"), range("3.13.0", "3.13.14")];
        assert_eq!(
            against_fixes("3.13.17", "python", &fixes, &ranges),
            Some((true, "3.13.14".into()))
        );
        assert_eq!(
            against_fixes("3.13.9", "python", &fixes, &ranges),
            Some((false, "3.13.14".into()))
        );
        // Another product's fixes say nothing about this library.
        assert_eq!(against_fixes("3.13.9", "openssl", &fixes, &ranges), None);
    }

    #[tokio::test]
    async fn a_tag_holding_a_fix_commit_is_fixed_and_the_answer_is_kept() {
        // Answers already cached: nothing goes to GitHub.
        sandogasa_cli::install_crypto_provider();
        let dir = tempfile::tempdir().unwrap();
        let copies = Copies::at(dir.path(), reqwest::Client::new(), None);
        let tags: Vec<String> = ["v3.13.16", "v3.13.17", "v3.13.9", "v3.14.0"]
            .map(String::from)
            .to_vec();
        copies.tags.store(
            &url_key("https://api.github.com/repos/python/cpython/tags"),
            &tags.join("\n"),
        );
        let sha = "0478bd83d82b0000000000000000000000000000";
        copies
            .contains
            .store(&url_key(&format!("python/cpython/v3.13.17/{sha}")), "true");
        let fix = OsvFix {
            id: "CVE-2026-7774".into(),
            vulns: Vec::new(),
            fixes: vec![osv::FixCommits {
                repo: "https://github.com/python/cpython".into(),
                commits: vec![sha.into()],
            }],
        };
        let verdict = copies
            .git_verdict(&fix, "CVE-2026-7774", "3.13.17")
            .await
            .expect("a verdict");
        assert!(verdict.fixed);
        assert_eq!(verdict.what, "commit 0478bd83d82b (in v3.13.17)");
    }
}
