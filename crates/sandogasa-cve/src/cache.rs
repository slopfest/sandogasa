// SPDX-License-Identifier: Apache-2.0 OR MIT

//! NVD answers, paced to NVD's rate limit and kept on disk for a day.
//!
//! NVD allows 5 requests per rolling 30 s without an API key and 50
//! with one; a refusal means the window is spent, so the cache waits
//! a whole window and asks once more, and records a CVE refused twice
//! so a run can say which bugs went unjudged for want of data rather
//! than because nothing applied.

use std::collections::HashMap;
use std::time::Duration;

use sandogasa_cli::cache::DiskCache;
use sandogasa_nvd::NvdClient;

/// Per-CVE NVD lookups with caching and rate limiting.
///
/// Wraps [`NvdClient`] with a cache of `map`ped responses keyed by
/// CVE ID, sleeping 6 seconds between requests to respect the
/// unauthenticated NVD rate limit (5 req / 30s). A failed fetch
/// prints a warning and caches nothing, so the caller skips that bug
/// (and a later bug for the same CVE retries the fetch).
pub struct NvdCache<T> {
    client: NvdClient,
    cache: HashMap<String, T>,
    disk: DiskCache,
    requests: u32,
    /// The pause between requests: NVD asks for 6 s without an API key
    /// (5 requests per rolling 30 s) and 0.6 s with one (50 per 30 s).
    interval: Duration,
    /// How long to wait before the one retry after NVD refuses a
    /// request; [`NVD_BACKOFF`] in a run, shorter in tests.
    backoff: Duration,
    /// CVEs NVD refused twice over — rate limited — so their bugs
    /// matched no check for want of data, not because none applied.
    pub refused: Vec<String>,
    verbose: bool,
}

/// NVD's unauthenticated window is 5 requests per 30 s; a refusal
/// means the window is spent, so wait a whole one out before asking
/// again.
pub const NVD_BACKOFF: Duration = Duration::from_secs(30);
/// Whether an NVD error is a refused API key: NVD answers `404 Not
/// Found` to an invalid or not-yet-activated key, so for a CVE that
/// exists a 404 is about the key.
pub fn nvd_refused_key(e: &reqwest::Error) -> bool {
    e.status().is_some_and(|s| s.as_u16() == 404)
}

/// Whether an NVD error is the service refusing to serve — its rate
/// limit (403, or 429 on newer deployments) or a momentary 503 — as
/// opposed to a CVE it has never heard of or a broken network.
pub fn nvd_throttled(e: &reqwest::Error) -> bool {
    e.status()
        .is_some_and(|s| matches!(s.as_u16(), 403 | 429 | 503))
}

/// How long an NVD answer, or an advisory page, may serve from disk.
pub const NVD_TTL: Duration = Duration::from_secs(24 * 60 * 60);
impl<T> NvdCache<T> {
    /// A cache for `tool` (its XDG cache directory holds the answers
    /// for a day), asking NVD with `api_key` when one is configured —
    /// which lifts the pace from one request per 6 s to one per 0.6 s.
    pub fn new(tool: &str, api_key: String, verbose: bool, refresh: bool) -> Self {
        Self::with(
            NvdClient::new().with_api_key(api_key),
            DiskCache::new(tool, "nvd", Some(NVD_TTL), refresh),
            NVD_BACKOFF,
            verbose,
        )
    }

    pub fn with(client: NvdClient, disk: DiskCache, backoff: Duration, verbose: bool) -> Self {
        let interval = if client.has_api_key() {
            Duration::from_millis(600)
        } else {
            Duration::from_secs(6)
        };
        Self {
            client,
            cache: HashMap::new(),
            disk,
            requests: 0,
            interval,
            backoff,
            refused: Vec::new(),
            verbose,
        }
    }

    /// NVD's answer for `cve_id`, asking once more after a refusal:
    /// the rate limit is a spent window, not a verdict on the CVE. A
    /// second refusal is recorded in `refused` so the run can say
    /// which bugs went unjudged for it.
    async fn fetch(&mut self, cve_id: &str) -> Option<String> {
        let mut attempt = 0;
        while attempt < 2 {
            match self.client.cve_json(cve_id).await {
                Ok(body) => return Some(body),
                // NVD answers 404 to an invalid or not-yet-activated
                // API key. Rather than fail every lookup of the run,
                // say so once and carry on without the key at the pace
                // that goes with having none.
                Err(e) if nvd_refused_key(&e) && self.client.has_api_key() => {
                    eprintln!(
                        "warning: NVD refused the configured API key (404: not accepted, or not \
                         yet activated by the mailed link); continuing without it at one request \
                         per 6 s — check the key in the tool's config"
                    );
                    self.client.clear_api_key();
                    self.interval = Duration::from_secs(6);
                    continue;
                }
                Err(e) if nvd_throttled(&e) && attempt == 0 => {
                    eprintln!(
                        "NVD refused {cve_id} ({}); waiting {} s before retrying",
                        e.status().map(|s| s.as_u16()).unwrap_or_default(),
                        self.backoff.as_secs()
                    );
                    tokio::time::sleep(self.backoff).await;
                }
                Err(e) if nvd_throttled(&e) => {
                    eprintln!("Warning: NVD refused {cve_id} again; its bugs go unjudged this run");
                    self.refused.push(cve_id.to_string());
                    return None;
                }
                Err(e) => {
                    eprintln!("Warning: failed to fetch {cve_id} from NVD: {e}");
                    return None;
                }
            }
            attempt += 1;
        }
        None
    }

    /// The cached value for `cve_id`, fetching and `map`ping the NVD
    /// response on a miss. `None` means the fetch failed (a warning
    /// has already been printed).
    pub async fn get(
        &mut self,
        cve_id: &str,
        map: impl FnOnce(sandogasa_nvd::models::CveResponse) -> T,
    ) -> Option<&T> {
        if !self.cache.contains_key(cve_id) {
            // A day-old answer on disk spares the request and the wait.
            if let Some(body) = self.disk.load(&format!("{cve_id}.json"))
                && let Ok(resp) = serde_json::from_str::<sandogasa_nvd::models::CveResponse>(&body)
            {
                if self.verbose {
                    eprintln!("Using cached NVD answer for {cve_id}...");
                }
                self.cache.insert(cve_id.to_string(), map(resp));
                return self.cache.get(cve_id);
            }
            // Pace to NVD's limit: 5 requests per 30 s bare, 50 keyed.
            if self.requests > 0 {
                tokio::time::sleep(self.interval).await;
            }
            self.requests += 1;

            if self.verbose {
                eprintln!("Querying NVD for {}...", cve_id);
            }
            let body = self.fetch(cve_id).await?;
            match serde_json::from_str::<sandogasa_nvd::models::CveResponse>(&body) {
                Ok(resp) => {
                    self.disk.store(&format!("{cve_id}.json"), &body);
                    self.cache.insert(cve_id.to_string(), map(resp));
                }
                Err(e) => {
                    eprintln!("Warning: NVD's answer for {cve_id} did not parse: {e}");
                    return None;
                }
            }
        }
        self.cache.get(cve_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nvd_cache_retries_once_after_a_refusal() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // The first request hits the rate limit; the retry is served.
        Mock::given(method("GET"))
            .and(query_param("cveId", "CVE-2026-1"))
            .respond_with(ResponseTemplate::new(403))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("cveId", "CVE-2026-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "vulnerabilities": []
            })))
            .mount(&server)
            .await;
        // Refused every time.
        Mock::given(method("GET"))
            .and(query_param("cveId", "CVE-2026-2"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let mut nvd = NvdCache::with(
            NvdClient::with_base_url(&server.uri()),
            DiskCache::at(None, false, Some(NVD_TTL)),
            Duration::from_millis(10),
            false,
        );
        assert!(
            nvd.get("CVE-2026-1", |r| r.vulnerabilities.len())
                .await
                .is_some()
        );
        assert!(nvd.refused.is_empty());
        assert!(
            nvd.get("CVE-2026-2", |r| r.vulnerabilities.len())
                .await
                .is_none()
        );
        assert_eq!(nvd.refused, vec!["CVE-2026-2".to_string()]);
        // Three refusals and one answer were requested: the retries happened.
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn nvd_cache_drops_a_refused_api_key() {
        use wiremock::matchers::{header, method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        // With the key: 404, NVD's answer to a key it does not know.
        Mock::given(method("GET"))
            .and(query_param("cveId", "CVE-2026-1"))
            .and(header("apiKey", "stale"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(query_param("cveId", "CVE-2026-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "vulnerabilities": []
            })))
            .mount(&server)
            .await;
        let mut nvd = NvdCache::with(
            NvdClient::with_base_url(&server.uri()).with_api_key("stale"),
            DiskCache::at(None, false, Some(NVD_TTL)),
            Duration::from_millis(10),
            false,
        );
        assert_eq!(nvd.interval, Duration::from_millis(600));
        assert!(
            nvd.get("CVE-2026-1", |r| r.vulnerabilities.len())
                .await
                .is_some()
        );
        assert!(!nvd.client.has_api_key());
        assert_eq!(nvd.interval, Duration::from_secs(6));
        assert!(nvd.refused.is_empty());
    }
}
