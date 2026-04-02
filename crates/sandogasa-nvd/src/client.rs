// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

use crate::models::CveResponse;

pub struct NvdClient {
    base_url: String,
    client: Client,
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
            client: Client::new(),
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
        }
    }

    /// Fetch a single CVE by ID from the NVD API.
    pub async fn cve(&self, cve_id: &str) -> Result<CveResponse, reqwest::Error> {
        self.client
            .get(format!("{}?cveId={cve_id}", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
