// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

const DISTGIT_BASE: &str = "https://src.fedoraproject.org";

pub struct DistGitClient {
    base_url: String,
    client: Client,
}

impl DistGitClient {
    pub fn new() -> Self {
        Self {
            base_url: DISTGIT_BASE.to_string(),
            client: Client::new(),
        }
    }

    /// Fetch the spec file for a package on a given dist-git branch.
    ///
    /// `package` is the source RPM name (e.g. "pcem").
    /// `branch` is the dist-git branch (e.g. "rawhide", "f43", "epel9").
    pub async fn fetch_spec(
        &self,
        package: &str,
        branch: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/rpms/{}/raw/{}/f/{}.spec",
            self.base_url, package, branch, package
        );
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let body = resp.text().await?;
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_default_base_url() {
        let client = DistGitClient::new();
        assert_eq!(client.base_url, "https://src.fedoraproject.org");
    }
}
