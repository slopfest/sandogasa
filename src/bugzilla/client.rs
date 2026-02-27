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

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/rest/{}", self.base_url, path.trim_start_matches('/'));
        let req = self.client.get(&url);
        if let Some(ref key) = self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }

    /// Fetch a single bug by numeric ID.
    pub async fn bug(&self, id: u64) -> Result<Bug, reqwest::Error> {
        self.bug_by_id(&id.to_string()).await
    }

    /// Fetch a single bug by alias (e.g. "CVE-FalsePositive-Unshipped") or numeric ID string.
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
    pub async fn search(&self, query: &str) -> Result<Vec<Bug>, reqwest::Error> {
        let resp: BugSearchResponse = self
            .request(&format!("bug?{query}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.bugs)
    }

    /// Fetch comments for a bug.
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
}
