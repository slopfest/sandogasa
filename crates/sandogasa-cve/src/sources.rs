// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where a CVE's fix is looked up beyond NVD, in one place.
//!
//! In order: GitHub's advisory database, asked by CVE; the advisories
//! NVD's references name, which GitHub may not have reviewed into its
//! database yet; and OSV, for the upstream databases GitHub does not
//! mirror. Every answer is kept in the caller's [`DiskCache`], and the
//! caller hears about each request that actually goes out, so a tool
//! can say what it is asking without repeating it for cached answers.
//!
//! The sources that say what a CVE is, not where it is fixed, are
//! asked the same way: Red Hat's security data and cve.org's record.

use sandogasa_cli::cache::{DiskCache, url_key};

use crate::advisory::{self, GhsaVuln};
use crate::cveorg;
use crate::osv::{self, OsvFix};

/// Where the sources are asked, when not at each service's own
/// address: a test's server on loopback, so that no test reaches the
/// network (`Sources::with_endpoints`). Each is a base URL replacing
/// the service's scheme and host (`http://127.0.0.1:8080`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Endpoints {
    /// In place of `https://access.redhat.com`.
    pub redhat: Option<String>,
    /// In place of `https://cveawg.mitre.org`, cve.org's API.
    pub cveorg: Option<String>,
    /// In place of `https://api.github.com`.
    pub github: Option<String>,
    /// In place of `https://api.osv.dev`.
    pub osv: Option<String>,
}

impl Endpoints {
    /// Red Hat's security data asked at `base`.
    pub fn with_redhat(mut self, base: impl Into<String>) -> Self {
        self.redhat = Some(base.into());
        self
    }

    /// cve.org's API asked at `base`.
    pub fn with_cveorg(mut self, base: impl Into<String>) -> Self {
        self.cveorg = Some(base.into());
        self
    }

    /// GitHub's API asked at `base`.
    pub fn with_github(mut self, base: impl Into<String>) -> Self {
        self.github = Some(base.into());
        self
    }

    /// OSV asked at `base`.
    pub fn with_osv(mut self, base: impl Into<String>) -> Self {
        self.osv = Some(base.into());
        self
    }

    /// `url` with its service's address replaced, when that service has
    /// an endpoint of its own; as it is otherwise.
    pub fn rebase(&self, url: &str) -> String {
        let services = [
            ("https://access.redhat.com", &self.redhat),
            ("https://cveawg.mitre.org", &self.cveorg),
            ("https://api.github.com", &self.github),
            ("https://api.osv.dev", &self.osv),
        ];
        for (own, base) in services {
            if let (Some(base), Some(rest)) = (base, url.strip_prefix(own)) {
                return format!("{}{rest}", base.trim_end_matches('/'));
            }
        }
        url.to_string()
    }
}

/// The advisory sources, with the client, cache and credentials they
/// share.
pub struct Sources<'a> {
    http: &'a reqwest::Client,
    cache: &'a DiskCache,
    github_token: Option<&'a str>,
    on_fetch: &'a dyn Fn(&str),
    endpoints: Endpoints,
}

impl<'a> Sources<'a> {
    /// Sources asking through `http`, keeping answers in `cache`, and
    /// using `github_token` (when given) to lift GitHub's rate limit.
    pub fn new(
        http: &'a reqwest::Client,
        cache: &'a DiskCache,
        github_token: Option<&'a str>,
    ) -> Self {
        Sources {
            http,
            cache,
            github_token,
            on_fetch: &|_| {},
            endpoints: Endpoints::default(),
        }
    }

    /// Ask the services at `endpoints` instead of their own addresses.
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Call `on_fetch` with a short description (`Asking OSV about
    /// CVE-2025-8194`) before each request that is not answered from
    /// the cache.
    pub fn reporting(mut self, on_fetch: &'a dyn Fn(&str)) -> Self {
        self.on_fetch = on_fetch;
        self
    }

    /// GitHub's advisory database, asked by CVE: each advisory's id and
    /// affected packages.
    pub async fn ghsa_records(&self, cve_id: &str) -> Vec<(String, Vec<GhsaVuln>)> {
        self.github(
            &advisory::ghsa_search_url(cve_id),
            &format!("Asking GitHub's advisory database about {cve_id}"),
        )
        .await
        .map(|json| advisory::ghsa_records(&json))
        .unwrap_or_default()
    }

    /// The advisories a CVE's references name. Each is asked of the
    /// global database by its GHSA id first, which states a range per
    /// affected series, and failing that of the project's own endpoint,
    /// where a fresh advisory lives before GitHub reviews it in, at the
    /// cost of one range for every series.
    pub async fn referenced_records(&self, references: &[String]) -> Vec<(String, Vec<GhsaVuln>)> {
        let mut out = Vec::new();
        for (owner, repo, ghsa_id) in advisory::repo_advisory_refs(references) {
            let records = self
                .github(
                    &advisory::ghsa_id_url(&ghsa_id),
                    &format!("Asking GitHub about {ghsa_id}"),
                )
                .await
                .map(|json| advisory::ghsa_records(&json))
                .unwrap_or_default();
            let records = if advisory::needs_repo_advisories(&records) {
                let url = advisory::repo_advisory_url(&owner, &repo, &ghsa_id);
                match self
                    .github(&url, &format!("Asking GitHub about {owner}/{repo}"))
                    .await
                {
                    Some(json) => advisory::repo_advisory_records(&json),
                    None => records,
                }
            } else {
                records
            };
            out.extend(records);
        }
        out
    }

    /// What OSV says about `cve_id`: its own record, with the upstream
    /// aliases it names merged in (see [`osv::upstream_aliases`]).
    pub async fn osv_fix(&self, cve_id: &str) -> Option<OsvFix> {
        let json = self.osv_record(cve_id).await?;
        let mut fix = osv::parse(&json);
        for alias in osv::upstream_aliases(&json) {
            let Some(other) = self
                .osv_record(&alias)
                .await
                .as_deref()
                .and_then(osv::parse)
            else {
                continue;
            };
            match &mut fix {
                Some(f) => osv::merge(f, other),
                None => fix = Some(other),
            }
        }
        fix
    }

    /// Every advisory record worth reading for a CVE, in GHSA's shape:
    /// GitHub's by CVE; the referenced ones when those name no affected
    /// package, which is what an entry GitHub has not curated yet looks
    /// like; and, when nothing so far has a patched version, OSV's
    /// upstream-ecosystem ranges under the OSV record's id. `osv` is
    /// the CVE's OSV answer when the caller has it already.
    pub async fn advisory_records(
        &self,
        cve_id: &str,
        references: &[String],
        osv: Option<Option<OsvFix>>,
    ) -> Vec<(String, Vec<GhsaVuln>)> {
        let mut records = self.ghsa_records(cve_id).await;
        if advisory::needs_repo_advisories(&records) {
            records.extend(self.referenced_records(references).await);
        }
        let patched = records
            .iter()
            .flat_map(|(_, v)| v)
            .any(|v| v.patched.is_some());
        if !patched {
            let fix = match osv {
                Some(fix) => fix,
                None => self.osv_fix(cve_id).await,
            };
            if let Some(fix) = fix
                && !fix.vulns.is_empty()
            {
                records.push((fix.id, fix.vulns));
            }
        }
        records
    }

    /// Red Hat's security data for `cve_id`, as JSON
    /// (see [`crate::severity::redhat_url`]); `None` when Red Hat has
    /// not assessed it or does not answer.
    pub async fn redhat_cve(&self, cve_id: &str) -> Option<String> {
        let url = crate::severity::redhat_url(cve_id);
        self.cached(&url, &format!("Asking Red Hat about {cve_id}"), false)
            .await
    }

    /// GitHub's advisory database asked by CVE, as JSON: what
    /// [`Self::ghsa_records`] reads the affected packages from and
    /// [`crate::severity::ghsa_rating`] the severity.
    pub async fn ghsa_json(&self, cve_id: &str) -> Option<String> {
        self.github(
            &advisory::ghsa_search_url(cve_id),
            &format!("Asking GitHub's advisory database about {cve_id}"),
        )
        .await
    }

    /// cve.org's record of `cve_id` (see [`crate::cveorg`]); `None`
    /// when it has published nothing under that id or does not answer.
    pub async fn cveorg(&self, cve_id: &str) -> Option<cveorg::Record> {
        let url = cveorg::record_url(cve_id);
        // Only a record is kept: a busy page sent with 200 would
        // otherwise hide the CVE's record for a day.
        let json = self
            .kept_if(
                &url,
                &format!("Asking cve.org about {cve_id}"),
                false,
                |json| cveorg::parse(json).is_some(),
            )
            .await?;
        cveorg::parse(&json)
    }

    /// One OSV record as JSON.
    async fn osv_record(&self, id: &str) -> Option<String> {
        let url = osv::vuln_url(id);
        self.cached(&url, &format!("Asking OSV about {id}"), false)
            .await
    }

    /// A GitHub API answer.
    async fn github(&self, url: &str, what: &str) -> Option<String> {
        self.cached(url, what, true).await
    }

    /// `url` from the cache, or fetched (and kept) when it is not
    /// there; `None` when it cannot be read.
    async fn cached(&self, url: &str, what: &str, github: bool) -> Option<String> {
        self.kept_if(url, what, github, |_| true).await
    }

    /// [`Self::cached`], keeping a fetched answer only when `keep`
    /// says it is one worth reading again.
    async fn kept_if(
        &self,
        url: &str,
        what: &str,
        github: bool,
        keep: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let url = &self.endpoints.rebase(url);
        let key = url_key(url);
        if let Some(text) = self.cache.load(&key) {
            return Some(text);
        }
        (self.on_fetch)(what);
        let text = if github {
            advisory::fetch_github(self.http, url, self.github_token).await
        } else {
            advisory::fetch(self.http, url).await
        }?;
        if keep(&text) {
            self.cache.store(&key, &text);
        }
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A cache holding `answers` as if fetched before.
    fn primed(dir: &std::path::Path, answers: &[(&str, &str)]) -> DiskCache {
        let cache = DiskCache::at(Some(dir.to_path_buf()), false, None);
        for (url, body) in answers {
            cache.store(&url_key(url), body);
        }
        cache
    }

    #[test]
    fn an_endpoint_replaces_only_its_own_service_s_address() {
        let e = Endpoints::default()
            .with_redhat("http://127.0.0.1:1/")
            .with_cveorg("http://127.0.0.1:2")
            .with_github("http://127.0.0.1:3")
            .with_osv("http://127.0.0.1:4");
        assert_eq!(
            e.rebase(&crate::severity::redhat_url("CVE-1")),
            "http://127.0.0.1:1/hydra/rest/securitydata/cve/CVE-1.json"
        );
        assert_eq!(
            e.rebase(&cveorg::record_url("CVE-1")),
            "http://127.0.0.1:2/api/cve/CVE-1"
        );
        assert_eq!(
            e.rebase(&advisory::ghsa_search_url("CVE-1")),
            "http://127.0.0.1:3/advisories?cve_id=CVE-1"
        );
        assert_eq!(
            e.rebase(&osv::vuln_url("CVE-1")),
            "http://127.0.0.1:4/v1/vulns/CVE-1"
        );
        // Unset, or another service: unchanged.
        let url = advisory::ghsa_search_url("CVE-1");
        assert_eq!(Endpoints::default().rebase(&url), url);
        assert_eq!(
            Endpoints::default()
                .with_redhat("http://x")
                .rebase("https://example.org/a"),
            "https://example.org/a"
        );
    }

    #[tokio::test]
    async fn osv_ranges_join_only_when_github_has_no_patched_version() {
        let dir = tempfile::tempdir().unwrap();
        let osv_json = r#"{"id": "PYSEC-2026-12", "affected": [
            {"package": {"ecosystem": "PyPI", "name": "requests"},
             "ranges": [{"type": "ECOSYSTEM",
               "events": [{"introduced": "0"}, {"fixed": "2.32.4"}]}]}]}"#;
        let cache = primed(
            dir.path(),
            &[
                (&advisory::ghsa_search_url("CVE-2026-1"), "[]"),
                (&osv::vuln_url("CVE-2026-1"), osv_json),
            ],
        );
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let fetched = RefCell::new(Vec::new());
        let note = |what: &str| fetched.borrow_mut().push(what.to_string());
        let sources = Sources::new(&http, &cache, None).reporting(&note);
        let records = sources.advisory_records("CVE-2026-1", &[], None).await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "PYSEC-2026-12");
        assert_eq!(records[0].1[0].patched.as_deref(), Some("2.32.4"));
        // Everything came from the cache: nothing was fetched or reported.
        assert!(fetched.borrow().is_empty(), "{:?}", fetched.borrow());

        // GitHub with a patched version: OSV is not consulted.
        let ghsa = r#"[{"ghsa_id": "GHSA-aaaa-bbbb-cccc", "vulnerabilities": [
            {"package": {"ecosystem": "pip", "name": "requests"},
             "vulnerable_version_range": "< 2.32.4",
             "first_patched_version": "2.32.4"}]}]"#;
        let dir2 = tempfile::tempdir().unwrap();
        let cache2 = primed(
            dir2.path(),
            &[(&advisory::ghsa_search_url("CVE-2026-2"), ghsa)],
        );
        let sources = Sources::new(&http, &cache2, None).reporting(&note);
        let records = sources.advisory_records("CVE-2026-2", &[], None).await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "GHSA-aaaa-bbbb-cccc");
        assert!(fetched.borrow().is_empty(), "{:?}", fetched.borrow());
    }

    #[tokio::test]
    async fn cve_org_s_record_is_read_from_its_kept_answer() {
        let dir = tempfile::tempdir().unwrap();
        let cache = primed(
            dir.path(),
            &[
                (
                    &cveorg::record_url("CVE-2026-1"),
                    r#"{"cveMetadata": {"cveId": "CVE-2026-1", "state": "REJECTED"},
                        "containers": {"cna": {}}}"#,
                ),
                // Kept, but not a record.
                (&cveorg::record_url("CVE-2026-2"), r#"{"error": "busy"}"#),
            ],
        );
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let fetched = RefCell::new(Vec::new());
        let note = |what: &str| fetched.borrow_mut().push(what.to_string());
        let sources = Sources::new(&http, &cache, None).reporting(&note);
        let record = sources.cveorg("CVE-2026-1").await.expect("a record");
        assert!(record.is_rejected());
        assert_eq!(sources.cveorg("CVE-2026-2").await, None);
        assert!(fetched.borrow().is_empty(), "{:?}", fetched.borrow());
    }

    #[tokio::test]
    async fn only_a_cve_org_answer_that_is_a_record_is_kept() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/cve/CVE-2026-1"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>busy</html>"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/cve/CVE-2026-2"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"cveMetadata": {"cveId": "CVE-2026-2", "state": "PUBLISHED"},
                    "containers": {"cna": {}}}"#,
            ))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::at(Some(dir.path().to_path_buf()), false, None);
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let endpoints = Endpoints::default().with_cveorg(server.uri());
        let sources = Sources::new(&http, &cache, None).with_endpoints(endpoints.clone());
        assert_eq!(sources.cveorg("CVE-2026-1").await, None);
        assert!(sources.cveorg("CVE-2026-2").await.is_some());
        let kept = |cve: &str| cache.load(&url_key(&endpoints.rebase(&cveorg::record_url(cve))));
        assert_eq!(kept("CVE-2026-1"), None, "a busy page is asked for again");
        assert!(kept("CVE-2026-2").is_some());
    }
}
