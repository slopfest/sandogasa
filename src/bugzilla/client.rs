// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

use super::models::{Bug, BugSearchResponse, Comment, CommentResponse};

pub struct BzClient {
    base_url: String,
    client: Client,
    api_key: Option<String>,
}

impl BzClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, key: String) -> Self {
        self.api_key = Some(key);
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/rest/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.auth(self.client.get(self.url(path)))
    }

    /// Fetch a single bug by numeric ID.
    #[allow(dead_code)]
    pub async fn bug(&self, id: u64) -> Result<Bug, reqwest::Error> {
        self.bug_by_id(&id.to_string()).await
    }

    /// Fetch a single bug by alias (e.g. "CVE-FalsePositive-Unshipped") or numeric ID string.
    #[allow(dead_code)]
    pub async fn bug_by_alias(&self, id_or_alias: &str) -> Result<Bug, reqwest::Error> {
        self.bug_by_id(id_or_alias).await
    }

    async fn bug_by_id(&self, id: &str) -> Result<Bug, reqwest::Error> {
        let resp: BugSearchResponse = self
            .request(&format!("bug/{id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.bugs.into_iter().next().unwrap())
    }

    /// Search bugs with a query string (e.g. "product=Fedora&component=kernel&status=NEW").
    /// Paginates through all results up to `max_results`. Pass 0 for no limit.
    pub async fn search(&self, query: &str, max_results: u64) -> Result<Vec<Bug>, reqwest::Error> {
        const PAGE_SIZE: u64 = 1000;
        let mut all_bugs = Vec::new();
        let mut offset: u64 = 0;

        loop {
            let limit = if max_results > 0 {
                PAGE_SIZE.min(max_results - offset as u64)
            } else {
                PAGE_SIZE
            };

            let resp: BugSearchResponse = self
                .request(&format!("bug?{query}&limit={limit}&offset={offset}"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            let total = resp.total_matches.unwrap_or(resp.bugs.len() as u64);
            all_bugs.extend(resp.bugs);

            offset = all_bugs.len() as u64;
            if offset >= total || (max_results > 0 && offset >= max_results) {
                break;
            }
        }

        Ok(all_bugs)
    }

    /// Fetch comments for a bug.
    #[allow(dead_code)]
    pub async fn comments(&self, bug_id: u64) -> Result<Vec<Comment>, reqwest::Error> {
        let resp: CommentResponse = self
            .request(&format!("bug/{bug_id}/comment"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp
            .bugs
            .into_values()
            .next()
            .map(|b| b.comments)
            .unwrap_or_default())
    }

    /// Update a bug. Requires an API key. The body is a JSON object with fields to update.
    pub async fn update(
        &self,
        id: u64,
        body: &serde_json::Value,
    ) -> Result<(), reqwest::Error> {
        self.auth(self.client.put(self.url(&format!("bug/{id}"))))
            .json(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}
