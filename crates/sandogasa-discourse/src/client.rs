// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use crate::models::{User, UserResponse};

pub struct DiscourseClient {
    base_url: String,
    client: Client,
    api_key: Option<String>,
    api_username: Option<String>,
}

impl DiscourseClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
            api_key: None,
            api_username: None,
        }
    }

    pub fn with_api_key(mut self, key: String, username: String) -> Self {
        self.api_key = Some(key);
        self.api_username = Some(username);
        self
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.api_key, &self.api_username) {
            (Some(key), Some(username)) => {
                req.header("Api-Key", key).header("Api-Username", username)
            }
            _ => req,
        }
    }

    /// Fetch a user profile by username.
    pub async fn user(&self, username: &str) -> Result<User, reqwest::Error> {
        let url = format!("{}/u/{}.json", self.base_url, username);
        let resp: UserResponse = self
            .auth(self.client.get(&url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ---- new / URL normalization ----

    #[test]
    fn new_trims_trailing_slash() {
        let client = DiscourseClient::new("https://discussion.fedoraproject.org/");
        assert_eq!(client.base_url, "https://discussion.fedoraproject.org");
    }

    #[test]
    fn new_preserves_url_without_trailing_slash() {
        let client = DiscourseClient::new("https://discussion.fedoraproject.org");
        assert_eq!(client.base_url, "https://discussion.fedoraproject.org");
    }

    #[test]
    fn new_trims_multiple_trailing_slashes() {
        let client = DiscourseClient::new("https://discussion.fedoraproject.org///");
        assert_eq!(client.base_url, "https://discussion.fedoraproject.org");
    }

    #[test]
    fn new_no_api_key_by_default() {
        let client = DiscourseClient::new("https://example.com");
        assert!(client.api_key.is_none());
        assert!(client.api_username.is_none());
    }

    // ---- with_api_key ----

    #[test]
    fn with_api_key_sets_key_and_username() {
        let client = DiscourseClient::new("https://example.com")
            .with_api_key("secret".to_string(), "admin".to_string());
        assert_eq!(client.api_key.as_deref(), Some("secret"));
        assert_eq!(client.api_username.as_deref(), Some("admin"));
    }

    // ---- user() ----

    #[tokio::test]
    async fn user_fetches_profile() {
        let server = MockServer::start().await;
        let client = DiscourseClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/u/mattdm.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {
                    "id": 1,
                    "username": "mattdm",
                    "name": "Matthew Miller",
                    "title": "Fedora Project Leader",
                    "timezone": "America/New_York",
                    "location": "Somerville, MA",
                    "last_posted_at": "2026-03-17T14:50:30.112Z",
                    "last_seen_at": "2026-03-22T05:36:12.902Z"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let user = client.user("mattdm").await.unwrap();
        assert_eq!(user.id, 1);
        assert_eq!(user.username, "mattdm");
        assert_eq!(user.name.as_deref(), Some("Matthew Miller"));
        assert_eq!(user.timezone.as_deref(), Some("America/New_York"));
        assert_eq!(user.location.as_deref(), Some("Somerville, MA"));
        assert!(user.last_posted_at.is_some());
        assert!(user.last_seen_at.is_some());
    }

    #[tokio::test]
    async fn user_with_status() {
        let server = MockServer::start().await;
        let client = DiscourseClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/u/alice.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {
                    "id": 42,
                    "username": "alice",
                    "status": {
                        "emoji": "palm_tree",
                        "description": "On vacation",
                        "ends_at": "2026-04-01T00:00:00.000Z"
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let user = client.user("alice").await.unwrap();
        let status = user.status.as_ref().unwrap();
        assert_eq!(status.emoji.as_deref(), Some("palm_tree"));
        assert_eq!(status.description.as_deref(), Some("On vacation"));
        assert!(status.ends_at.is_some());
    }

    #[tokio::test]
    async fn user_without_optional_fields() {
        let server = MockServer::start().await;
        let client = DiscourseClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/u/newuser.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {
                    "id": 99,
                    "username": "newuser"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let user = client.user("newuser").await.unwrap();
        assert_eq!(user.id, 99);
        assert!(user.timezone.is_none());
        assert!(user.location.is_none());
        assert!(user.last_posted_at.is_none());
        assert!(user.status.is_none());
    }

    #[tokio::test]
    async fn user_sends_auth_headers() {
        let server = MockServer::start().await;
        let client = DiscourseClient::new(&server.uri())
            .with_api_key("mykey".to_string(), "admin".to_string());

        Mock::given(method("GET"))
            .and(path("/u/someone.json"))
            .and(header("Api-Key", "mykey"))
            .and(header("Api-Username", "admin"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {
                    "id": 5,
                    "username": "someone"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let user = client.user("someone").await.unwrap();
        assert_eq!(user.username, "someone");
    }

    #[tokio::test]
    async fn user_returns_error_on_not_found() {
        let server = MockServer::start().await;
        let client = DiscourseClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/u/nonexistent.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.user("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn user_returns_error_on_server_failure() {
        let server = MockServer::start().await;
        let client = DiscourseClient::new(&server.uri());

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client.user("anyone").await;
        assert!(result.is_err());
    }
}
