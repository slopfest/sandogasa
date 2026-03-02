// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

use super::models::{Update, UpdatesResponse};

const BODHI_API_BASE: &str = "https://bodhi.fedoraproject.org";

pub struct BodhiClient {
    base_url: String,
    client: Client,
}

impl BodhiClient {
    pub fn new() -> Self {
        Self {
            base_url: BODHI_API_BASE.to_string(),
            client: Client::new(),
        }
    }

    /// Fetch updates for a given package on a given release.
    ///
    /// `package` is the source package name (e.g. "freerdp").
    /// `release` is the Bodhi release name (e.g. "F42", "EPEL-9").
    /// `statuses` filters by update status (e.g. &["stable", "testing"]).
    ///
    /// Returns all matching updates, paginating through all pages.
    pub async fn updates_for_package(
        &self,
        package: &str,
        release: &str,
        statuses: &[&str],
    ) -> Result<Vec<Update>, reqwest::Error> {
        let mut all_updates = Vec::new();
        let mut page = 1;

        loop {
            let status_params: String = statuses
                .iter()
                .map(|s| format!("&status={s}"))
                .collect();
            let url = format!(
                "{}/updates/?packages={}&releases={}{}&rows_per_page=100&page={}",
                self.base_url, package, release, status_params, page
            );

            let resp: UpdatesResponse = self
                .client
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            all_updates.extend(resp.updates);

            if page >= resp.pages {
                break;
            }
            page += 1;
        }

        Ok(all_updates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_default_base_url() {
        let client = BodhiClient::new();
        assert_eq!(client.base_url, "https://bodhi.fedoraproject.org");
    }
}
