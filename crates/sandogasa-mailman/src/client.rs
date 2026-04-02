// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

use crate::models::{Email, PaginatedResponse, obfuscate_email};

const HYPERKITTY_BASE: &str = "https://lists.fedoraproject.org/archives";

pub struct MailmanClient {
    base_url: String,
    client: Client,
}

impl Default for MailmanClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MailmanClient {
    pub fn new() -> Self {
        Self {
            base_url: HYPERKITTY_BASE.to_string(),
            client: Client::new(),
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
        }
    }

    /// Find a sender's mailman_id by scanning recent emails on a list.
    ///
    /// Searches through recent emails looking for one sent from any of the
    /// given `emails`. Returns the mailman_id if found. Scans up to
    /// `max_pages` pages, checking all email addresses on each page
    /// before moving to the next.
    pub async fn find_sender_id(
        &self,
        list: &str,
        emails: &[String],
        max_pages: u32,
    ) -> Result<Option<String>, reqwest::Error> {
        let obfuscated: Vec<String> = emails.iter().map(|e| obfuscate_email(e)).collect();

        // HyperKitty returns newest emails first (page 1 = most recent).
        for page in 1..=max_pages {
            let url = format!(
                "{}/api/list/{}/emails/?format=json&page={}",
                self.base_url, list, page
            );
            let response = self.client.get(&url).send().await?.error_for_status()?;

            // Skip pages that don't return valid JSON (e.g. rate-limit pages).
            let resp: PaginatedResponse<Email> = match response.json().await {
                Ok(r) => r,
                Err(_) => continue,
            };

            for e in &resp.results {
                if let Some(sender) = &e.sender
                    && obfuscated.iter().any(|o| o == &sender.address)
                {
                    return Ok(Some(sender.mailman_id.clone()));
                }
            }

            if resp.next.is_none() {
                break;
            }
        }

        Ok(None)
    }

    /// Fetch recent emails by a sender across all lists.
    ///
    /// Requires the sender's mailman_id (obtained via `find_sender_id`).
    /// Returns up to one page of results (most recent first).
    pub async fn sender_emails(
        &self,
        mailman_id: &str,
        limit: u32,
    ) -> Result<Vec<Email>, reqwest::Error> {
        let url = format!(
            "{}/api/sender/{}/emails/?format=json&page=1",
            self.base_url, mailman_id
        );
        let resp: PaginatedResponse<Email> = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        Ok(resp.results.into_iter().take(limit as usize).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn new_uses_default_base_url() {
        let client = MailmanClient::new();
        assert_eq!(client.base_url, "https://lists.fedoraproject.org/archives");
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = MailmanClient::with_base_url("https://example.com/archives/");
        assert_eq!(client.base_url, "https://example.com/archives");
    }

    #[tokio::test]
    async fn find_sender_id_found() {
        let server = MockServer::start().await;
        let client = MailmanClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/api/list/.*/emails/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 2,
                "next": null,
                "previous": null,
                "results": [
                    {
                        "message_id_hash": "AAA",
                        "subject": "Other post",
                        "sender": {
                            "address": "other (a) example.com",
                            "mailman_id": "other-id"
                        }
                    },
                    {
                        "message_id_hash": "BBB",
                        "subject": "My post",
                        "sender": {
                            "address": "alice (a) example.com",
                            "mailman_id": "alice-id-123"
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let id = client
            .find_sender_id("test@example.com", &["alice@example.com".to_string()], 3)
            .await
            .unwrap();
        assert_eq!(id.as_deref(), Some("alice-id-123"));
    }

    #[tokio::test]
    async fn find_sender_id_not_found() {
        let server = MockServer::start().await;
        let client = MailmanClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/api/list/.*/emails/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 1,
                "next": null,
                "previous": null,
                "results": [
                    {
                        "message_id_hash": "AAA",
                        "subject": "Other post",
                        "sender": {
                            "address": "other (a) example.com",
                            "mailman_id": "other-id"
                        }
                    }
                ]
            })))
            .mount(&server)
            .await;

        let id = client
            .find_sender_id("test@example.com", &["nobody@example.com".to_string()], 1)
            .await
            .unwrap();
        assert!(id.is_none());
    }

    #[tokio::test]
    async fn sender_emails_returns_results() {
        let server = MockServer::start().await;
        let client = MailmanClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/api/sender/.*/emails/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 50,
                "next": null,
                "previous": null,
                "results": [
                    {
                        "message_id_hash": "AAA",
                        "subject": "Latest post",
                        "date": "2026-03-23T12:00:00+00:00",
                        "sender_name": "Alice",
                        "mailinglist": "https://example.com/api/list/devel@example.com/"
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let emails = client.sender_emails("alice-id", 5).await.unwrap();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].subject, "Latest post");
    }

    #[tokio::test]
    async fn sender_emails_respects_limit() {
        let server = MockServer::start().await;
        let client = MailmanClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/api/sender/.*/emails/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 100,
                "next": null,
                "previous": null,
                "results": [
                    {"message_id_hash": "A", "subject": "Post 1"},
                    {"message_id_hash": "B", "subject": "Post 2"},
                    {"message_id_hash": "C", "subject": "Post 3"}
                ]
            })))
            .mount(&server)
            .await;

        let emails = client.sender_emails("id", 2).await.unwrap();
        assert_eq!(emails.len(), 2);
    }
}
