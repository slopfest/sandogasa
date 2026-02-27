use reqwest::Client;

use super::models::CveResponse;

pub struct NvdClient {
    client: Client,
}

const NVD_API_BASE: &str = "https://services.nvd.nist.gov/rest/json/cves/2.0";

impl NvdClient {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    /// Fetch a single CVE by ID from the NVD API.
    pub async fn cve(&self, cve_id: &str) -> Result<CveResponse, reqwest::Error> {
        self.client
            .get(format!("{NVD_API_BASE}?cveId={cve_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
    }
}
