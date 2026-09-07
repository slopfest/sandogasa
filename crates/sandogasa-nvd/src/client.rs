// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use crate::models::CveResponse;

pub struct NvdClient {
    base_url: String,
    client: Client,
    /// An NVD API key, sent as the `apiKey` header. Free to request at
    /// <https://nvd.nist.gov/developers/request-an-api-key>; it raises
    /// the rate limit from 5 requests per 30 s to 50.
    api_key: Option<String>,
}

const NVD_API_BASE: &str = "https://services.nvd.nist.gov/rest/json/cves/2.0";

impl Default for NvdClient {
    fn default() -> Self {
        Self::new()
    }
}

impl NvdClient {
    pub fn new() -> Self {
        Self {
            base_url: NVD_API_BASE.to_string(),
            client: build_http_client(),
            api_key: None,
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: build_http_client(),
            api_key: None,
        }
    }

    /// Send `key` as the `apiKey` header on every request. An empty
    /// key is no key.
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.api_key = (!key.is_empty()).then_some(key);
        self
    }

    /// Whether a key is set, so callers can pace requests to the
    /// higher limit it buys.
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Drop the key, for a caller that has seen NVD refuse it. NVD
    /// answers `404 Not Found` to an invalid or not-yet-activated key,
    /// so a 404 for a CVE that exists is the key's, not the CVE's.
    pub fn clear_api_key(&mut self) {
        self.api_key = None;
    }

    fn request(&self, cve_id: &str) -> reqwest::RequestBuilder {
        let req = self.client.get(format!("{}?cveId={cve_id}", self.base_url));
        match &self.api_key {
            Some(key) => req.header("apiKey", key),
            None => req,
        }
    }

    /// Fetch a single CVE by ID from the NVD API.
    pub async fn cve(&self, cve_id: &str) -> Result<CveResponse, reqwest::Error> {
        self.request(cve_id)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }

    /// The raw JSON body NVD returns for `cve_id`, for callers that
    /// keep it — a disk cache that outlives the process, say — and
    /// parse it into a [`CveResponse`] themselves.
    pub async fn cve_json(&self, cve_id: &str) -> Result<String, reqwest::Error> {
        self.request(cve_id)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await
    }
}

/// Build the crate's HTTP client with the shared sandogasa defaults
/// (user agent, request timeout, crypto provider). Panics only where
/// `Client::new()` would too (TLS backend init).
fn build_http_client() -> Client {
    sandogasa_cli::http::builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .build()
    .expect("build reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn an_api_key_rides_as_a_header() {
        use wiremock::matchers::header;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("cveId", "CVE-2026-1"))
            .and(header("apiKey", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"vulnerabilities": []}"#))
            .mount(&server)
            .await;
        let keyed = NvdClient::with_base_url(&server.uri()).with_api_key("secret");
        assert!(keyed.has_api_key());
        assert!(keyed.cve_json("CVE-2026-1").await.is_ok());
        // Without the header the mock does not match: no key, no answer.
        let bare = NvdClient::with_base_url(&server.uri()).with_api_key("");
        assert!(!bare.has_api_key());
        assert!(bare.cve_json("CVE-2026-1").await.is_err());
    }

    #[test]
    fn new_uses_default_base_url() {
        let client = NvdClient::new();
        assert_eq!(client.base_url, NVD_API_BASE);
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = NvdClient::with_base_url("http://localhost:8080/");
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[tokio::test]
    async fn cve_returns_parsed_response() {
        let server = MockServer::start().await;
        let client = NvdClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(query_param("cveId", "CVE-2025-1234"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "vulnerabilities": [{
                    "cve": {
                        "id": "CVE-2025-1234",
                        "sourceIdentifier": "security@example.com",
                        "descriptions": [{"lang": "en", "value": "Test vuln"}],
                        "configurations": [],
                        "references": []
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let resp = client.cve("CVE-2025-1234").await.unwrap();
        assert_eq!(resp.vulnerabilities.len(), 1);
        assert_eq!(resp.vulnerabilities[0].cve.id, "CVE-2025-1234");
        assert_eq!(
            resp.vulnerabilities[0].cve.descriptions[0].value,
            "Test vuln"
        );
    }

    #[tokio::test]
    async fn cve_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = NvdClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.cve("CVE-9999-0000").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cve_returns_error_on_server_error() {
        let server = MockServer::start().await;
        let client = NvdClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client.cve("CVE-2025-1234").await;
        assert!(result.is_err());
    }
}
