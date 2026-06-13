// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::time::Duration;

use reqwest::Client;

use crate::models::{
    BodhiRelease, BugFeedbackItem, Comment, CommentsResponse, ReleasesResponse,
    SingleCommentResponse, SingleUpdateResponse, Update, UpdatesResponse,
};

const BODHI_API_BASE: &str = "https://bodhi.fedoraproject.org";

/// Upper bound on any single Bodhi HTTP request. Bodhi routinely
/// takes 5–30s on larger queries, so this is deliberately
/// generous; its job is to catch genuinely hung connections
/// rather than to bound latency.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

pub struct BodhiClient {
    base_url: String,
    client: Client,
    token: Option<String>,
}

impl Default for BodhiClient {
    fn default() -> Self {
        Self::new()
    }
}

fn build_http_client() -> Client {
    sandogasa_cli::install_crypto_provider();
    Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        // Bodhi authenticates write requests by the `auth_tkt`
        // cookie set by `GET /oidc/login-token`, and binds the
        // CSRF token to the session cookie — both must persist
        // across the login -> csrf -> POST sequence.
        .cookie_store(true)
        .build()
        .expect("build reqwest client")
}

impl BodhiClient {
    pub fn new() -> Self {
        Self {
            base_url: BODHI_API_BASE.to_string(),
            client: build_http_client(),
            token: None,
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: build_http_client(),
            token: None,
        }
    }

    /// Attach an OIDC bearer token for authenticated requests
    /// (e.g. [`Self::comment`]). See [`crate::auth`] for obtaining
    /// one from the bodhi CLI's session cache.
    ///
    /// Refuses (returns an error) if the client's base URL would
    /// send the token over plaintext `http` to a non-loopback
    /// host; see [`sandogasa_cli::ensure_secure_url`].
    pub fn with_token(mut self, token: String) -> Result<Self, Box<dyn std::error::Error>> {
        sandogasa_cli::ensure_secure_url(&self.base_url)?;
        self.token = Some(token);
        Ok(self)
    }

    /// Add the bearer token to a request, if one is attached.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.token {
            req.bearer_auth(token)
        } else {
            req
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
            let status_params: String = statuses.iter().map(|s| format!("&status={s}")).collect();
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

    /// Fetch active Fedora and EPEL releases from the Bodhi API.
    ///
    /// Returns releases with state "current", "pending", or "frozen",
    /// excluding Flatpak, Container, ELN, and EPEL-Next variants.
    pub async fn active_releases(&self) -> Result<Vec<BodhiRelease>, reqwest::Error> {
        let url = format!("{}/releases/?chrome=0&rows_per_page=100", self.base_url);

        let resp: ReleasesResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let active: Vec<BodhiRelease> = resp
            .releases
            .into_iter()
            .filter(|r| matches!(r.state.as_str(), "current" | "pending" | "frozen"))
            .filter(|r| matches!(r.id_prefix.as_str(), "FEDORA" | "FEDORA-EPEL") && r.name != "ELN")
            .collect();

        Ok(active)
    }

    /// Fetch a single update by its alias (e.g. "FEDORA-EPEL-2026-f9eaa11e18").
    pub async fn update_by_alias(&self, alias: &str) -> Result<Update, reqwest::Error> {
        let url = format!("{}/updates/{}", self.base_url, alias);
        let resp: SingleUpdateResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.update)
    }

    /// Fetch updates submitted by a user, optionally filtered to
    /// a submission-date window.
    ///
    /// `submitted_since` and `submitted_before` map to Bodhi's
    /// `submitted_since` / `submitted_before` query params — pass
    /// them to let Bodhi narrow the result set server-side rather
    /// than client-filtering a huge response. Both are optional:
    /// omitting them yields the unfiltered "all updates by user"
    /// behaviour.
    ///
    /// Note: filtering by submission date misses updates
    /// submitted before the window that transitioned
    /// (testing/stable) inside it. For activity reports the
    /// caller should widen `submitted_since` with a reasonable
    /// buffer (days to weeks) if that edge case matters.
    ///
    /// Paginates at 100 rows per request until either `limit`
    /// updates have been collected or the last page is reached.
    /// Smaller per-request windows avoid Bodhi's tendency to
    /// time out on very large single fetches.
    ///
    /// `on_page` is invoked after each successful page with
    /// `(page_number, total_pages, running_count)` so callers
    /// can stream progress to the user.
    pub async fn updates_for_user<F>(
        &self,
        username: &str,
        limit: u32,
        submitted_since: Option<chrono::NaiveDate>,
        submitted_before: Option<chrono::NaiveDate>,
        mut on_page: F,
    ) -> Result<Vec<Update>, reqwest::Error>
    where
        F: FnMut(u64, u64, usize),
    {
        let mut all = Vec::new();
        let mut page = 1;
        let date_filters = {
            let mut s = String::new();
            if let Some(d) = submitted_since {
                s.push_str(&format!("&submitted_since={d}"));
            }
            if let Some(d) = submitted_before {
                s.push_str(&format!("&submitted_before={d}"));
            }
            s
        };
        loop {
            let url = format!(
                "{}/updates/?user={}{}&rows_per_page=100&chrome=0&page={}",
                self.base_url, username, date_filters, page
            );
            let resp: UpdatesResponse = self
                .client
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            all.extend(resp.updates);
            on_page(resp.page, resp.pages, all.len());
            if all.len() >= limit as usize || page >= resp.pages {
                break;
            }
            page += 1;
        }
        all.truncate(limit as usize);
        Ok(all)
    }

    /// Fetch the most recent comments by a user.
    ///
    /// Returns up to `limit` comments, most recent first.
    pub async fn comments_for_user(
        &self,
        username: &str,
        limit: u32,
    ) -> Result<Vec<Comment>, reqwest::Error> {
        let url = format!(
            "{}/comments/?user={}&rows_per_page={}&chrome=0",
            self.base_url, username, limit
        );
        let resp: CommentsResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.comments)
    }

    /// Exchange the bearer token for Bodhi's `auth_tkt` session
    /// cookie via `GET /oidc/login-token`. Bodhi authenticates
    /// write requests by that cookie, not by the Authorization
    /// header on the request itself — posting without it fails
    /// with "You must provide an author". Mirrors bodhi-client's
    /// `ensure_auth`. The cookie lands in this client's cookie
    /// store and rides along on subsequent requests.
    async fn login(&self) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("{}/oidc/login-token", self.base_url);
        let resp = crate::auth::send_with_retry("bodhi login", || {
            self.auth(self.client.get(&url))
                .header(reqwest::header::ACCEPT, "application/json")
        })
        .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "bodhi login failed (HTTP {status}): {body}; \
                 re-authenticate with the bodhi CLI"
            )
            .into());
        }
        Ok(())
    }

    /// Fetch a CSRF token for write requests, mirroring what the
    /// Python bodhi-client bindings do before each POST.
    async fn csrf(&self) -> Result<String, Box<dyn std::error::Error>> {
        #[derive(serde::Deserialize)]
        struct CsrfResponse {
            csrf_token: String,
        }
        let url = format!("{}/csrf", self.base_url);
        let resp: CsrfResponse = crate::auth::send_with_retry("bodhi csrf fetch", || {
            self.auth(self.client.get(&url))
                .header(reqwest::header::ACCEPT, "application/json")
        })
        .await?
        .error_for_status()?
        .json()
        .await?;
        Ok(resp.csrf_token)
    }

    /// Post a comment on an update, with overall karma and
    /// optional per-bug feedback (the web UI's per-bug thumbs
    /// up/down). Requires a bearer token ([`Self::with_token`]).
    ///
    /// `karma` and each feedback karma must be -1, 0, or +1.
    /// Non-2xx responses are surfaced with the response body,
    /// which carries Bodhi's validation errors. Check the
    /// response's `caveats` for server-side adjustments (e.g.
    /// karma is silently zeroed on one's own updates).
    ///
    /// The body is form-encoded with *flattened* feedback keys
    /// (`bug_feedback.0.bug_id=...`), matching the web UI:
    /// Bodhi's comment schema runs colander `unflatten` over the
    /// body, which silently drops a nested `bug_feedback` array.
    pub async fn comment(
        &self,
        update_alias: &str,
        text: &str,
        karma: i32,
        bug_feedback: &[BugFeedbackItem],
    ) -> Result<SingleCommentResponse, Box<dyn std::error::Error>> {
        self.login().await?;
        let csrf_token = self.csrf().await?;
        let mut form: Vec<(String, String)> = vec![
            ("update".to_string(), update_alias.to_string()),
            ("text".to_string(), text.to_string()),
            ("karma".to_string(), karma.to_string()),
            ("csrf_token".to_string(), csrf_token),
        ];
        for (i, fb) in bug_feedback.iter().enumerate() {
            form.push((format!("bug_feedback.{i}.bug_id"), fb.bug_id.to_string()));
            form.push((format!("bug_feedback.{i}.karma"), fb.karma.to_string()));
        }
        // The POST itself is deliberately not auto-retried: a
        // transport error after the server processed the request
        // would double-post the comment. The retried login/csrf
        // steps above absorb most transient failures.
        let url = format!("{}/comments/", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&form)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("posting comment failed (HTTP {status}): {body}").into());
        }
        let resp: SingleCommentResponse = resp.json().await?;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn new_uses_default_base_url() {
        let client = BodhiClient::new();
        assert_eq!(client.base_url, "https://bodhi.fedoraproject.org");
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = BodhiClient::with_base_url("http://localhost:8080/");
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[tokio::test]
    async fn updates_for_package_single_page() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [
                    {
                        "alias": "FEDORA-2026-abc",
                        "status": "stable",
                        "builds": [{"nvr": "freerdp-3.23.0-1.fc42"}],
                        "bugs": [],
                        "release": {"name": "F42"}
                    }
                ],
                "total": 1,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let updates = client
            .updates_for_package("freerdp", "F42", &["stable"])
            .await
            .unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].alias, "FEDORA-2026-abc");
    }

    #[tokio::test]
    async fn updates_for_package_empty() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [],
                "total": 0,
                "page": 1,
                "pages": 0
            })))
            .mount(&server)
            .await;

        let updates = client
            .updates_for_package("nonexistent", "F42", &["stable", "testing"])
            .await
            .unwrap();
        assert!(updates.is_empty());
    }

    #[tokio::test]
    async fn active_releases_filters_correctly() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/releases/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [
                    {"name": "F43", "branch": "f43", "id_prefix": "FEDORA", "state": "current"},
                    {"name": "EPEL-9", "branch": "epel9", "id_prefix": "FEDORA-EPEL", "state": "current"},
                    {"name": "ELN", "branch": "eln", "id_prefix": "FEDORA", "state": "current"},
                    {"name": "F42F", "branch": "f42f", "id_prefix": "FEDORA-FLATPAK", "state": "current"},
                    {"name": "F40", "branch": "f40", "id_prefix": "FEDORA", "state": "archived"}
                ],
                "total": 5,
                "page": 1,
                "pages": 1
            })))
            .mount(&server)
            .await;

        let releases = client.active_releases().await.unwrap();
        let names: Vec<_> = releases.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["F43", "EPEL-9"]);
    }

    // ---- update_by_alias ----

    #[tokio::test]
    async fn update_by_alias_returns_update() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/FEDORA-EPEL-2026-abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "update": {
                    "alias": "FEDORA-EPEL-2026-abc123",
                    "status": "testing",
                    "from_tag": "epel9-build-side-133287",
                    "builds": [
                        {"nvr": "rust-uucore-0.0.28-2.el9"}
                    ],
                    "bugs": [],
                    "release": {"name": "EPEL-9"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let update = client
            .update_by_alias("FEDORA-EPEL-2026-abc123")
            .await
            .unwrap();
        assert_eq!(update.alias, "FEDORA-EPEL-2026-abc123");
        assert_eq!(update.from_tag.as_deref(), Some("epel9-build-side-133287"));
        assert_eq!(update.builds.len(), 1);
        assert_eq!(update.builds[0].nvr, "rust-uucore-0.0.28-2.el9");
    }

    #[tokio::test]
    async fn update_by_alias_without_side_tag() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/FEDORA-2026-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "update": {
                    "alias": "FEDORA-2026-xyz",
                    "status": "stable",
                    "builds": [
                        {"nvr": "foo-1.0-1.fc42"},
                        {"nvr": "bar-2.0-1.fc42"}
                    ],
                    "bugs": [],
                    "release": {"name": "F42"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let update = client.update_by_alias("FEDORA-2026-xyz").await.unwrap();
        assert_eq!(update.alias, "FEDORA-2026-xyz");
        assert!(update.from_tag.is_none());
        assert_eq!(update.builds.len(), 2);
    }

    // ---- updates_for_user ----

    #[tokio::test]
    async fn updates_for_user_returns_updates() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [
                    {
                        "alias": "FEDORA-2026-b600f85be9",
                        "status": "testing",
                        "builds": [{"nvr": "python-puzpy-0.5.0-2.fc44"}],
                        "bugs": [],
                        "release": {"name": "F44"},
                        "date_submitted": "2026-03-20 23:44:44"
                    }
                ],
                "total": 1,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let (updates, pages) = {
            use std::sync::{Arc, Mutex};
            let pages = Arc::new(Mutex::new(Vec::new()));
            let pages_capture = Arc::clone(&pages);
            let updates = client
                .updates_for_user("salimma", 1, None, None, move |p, t, n| {
                    pages_capture.lock().unwrap().push((p, t, n));
                })
                .await
                .unwrap();
            let pages = Arc::try_unwrap(pages).unwrap().into_inner().unwrap();
            (updates, pages)
        };
        assert_eq!(updates.len(), 1);
        // Callback fired at least once with a running count.
        assert!(!pages.is_empty());
        assert_eq!(updates[0].alias, "FEDORA-2026-b600f85be9");
        assert_eq!(updates[0].release.as_ref().unwrap().name, "F44");
    }

    #[tokio::test]
    async fn updates_for_user_empty() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [],
                "total": 0,
                "page": 1,
                "pages": 0
            })))
            .mount(&server)
            .await;

        let updates = client
            .updates_for_user("nobody", 1, None, None, |_, _, _| {})
            .await
            .unwrap();
        assert!(updates.is_empty());
    }

    // ---- comments_for_user ----

    #[tokio::test]
    async fn comments_for_user_returns_comments() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/comments/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "comments": [
                    {
                        "id": 4559905,
                        "text": "Testing feedback",
                        "karma": 1,
                        "timestamp": "2026-02-24 11:17:59",
                        "author": "salimma",
                        "update_alias": "FEDORA-EPEL-2026-8e235e20a2"
                    }
                ],
                "total": 1,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let comments = client.comments_for_user("salimma", 1).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].karma, 1);
        assert_eq!(
            comments[0].update_alias.as_deref(),
            Some("FEDORA-EPEL-2026-8e235e20a2")
        );
    }

    // ---- comment ----

    #[tokio::test]
    async fn comment_posts_karma_and_bug_feedback() {
        use wiremock::matchers::{body_string_contains, header, path};

        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri())
            .with_token("tok-123".to_string())
            .unwrap();

        Mock::given(method("GET"))
            .and(path("/oidc/login-token"))
            .and(header("authorization", "Bearer tok-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/csrf"))
            .and(header("authorization", "Bearer tok-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "csrf_token": "csrf-abc"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/comments/"))
            .and(header("authorization", "Bearer tok-123"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("update=FEDORA-2026-94cb04410a"))
            .and(body_string_contains("text=works+for+me"))
            .and(body_string_contains("karma=1"))
            .and(body_string_contains("csrf_token=csrf-abc"))
            // Flattened, web-UI-style feedback keys: a nested
            // JSON array is silently dropped by the server's
            // colander unflatten step.
            .and(body_string_contains("bug_feedback.0.bug_id=100"))
            .and(body_string_contains("bug_feedback.0.karma=1"))
            .and(body_string_contains("bug_feedback.1.bug_id=200"))
            .and(body_string_contains("bug_feedback.1.karma=-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "comment": {
                    "id": 4672763,
                    "text": "works for me",
                    "karma": 1,
                    "author": "salimma",
                    "update_alias": "FEDORA-2026-94cb04410a"
                },
                "caveats": [
                    {"name": "karma",
                     "description": "You may not give karma to your own updates."}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let feedback = vec![
            BugFeedbackItem {
                bug_id: 100,
                karma: 1,
            },
            BugFeedbackItem {
                bug_id: 200,
                karma: -1,
            },
        ];
        let resp = client
            .comment("FEDORA-2026-94cb04410a", "works for me", 1, &feedback)
            .await
            .unwrap();
        assert_eq!(resp.comment.id, 4672763);
        assert_eq!(resp.comment.karma, 1);
        assert_eq!(resp.caveats.len(), 1);
        assert!(
            resp.caveats[0].description.contains("your own updates"),
            "{}",
            resp.caveats[0].description
        );
    }

    #[tokio::test]
    async fn comment_logs_in_and_carries_session_cookie() {
        use wiremock::matchers::{header, path};

        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri())
            .with_token("tok-123".to_string())
            .unwrap();

        // Bodhi authenticates the POST by the auth_tkt cookie set
        // by GET /oidc/login-token (not by the Authorization
        // header); without it the server rejects with "You must
        // provide an author". The CSRF token is likewise bound to
        // the session cookie, so cookies must persist across the
        // login -> csrf -> post sequence.
        Mock::given(method("GET"))
            .and(path("/oidc/login-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("set-cookie", "auth_tkt=tkt-xyz; Path=/")
                    .set_body_json(serde_json::json!({})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/csrf"))
            .and(header("cookie", "auth_tkt=tkt-xyz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"csrf_token": "csrf-abc"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/comments/"))
            .and(header("cookie", "auth_tkt=tkt-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "comment": {"id": 1, "karma": 0}
            })))
            .expect(1)
            .mount(&server)
            .await;

        client
            .comment("FEDORA-2026-94cb04410a", "", 0, &[])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn comment_surfaces_validation_errors() {
        use wiremock::matchers::path;

        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri())
            .with_token("tok-123".to_string())
            .unwrap();

        Mock::given(method("GET"))
            .and(path("/oidc/login-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/csrf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "csrf_token": "csrf-abc"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/comments/"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "status": "error",
                "errors": [{"location": "body", "name": "karma",
                            "description": "you may not give karma to your own updates"}]
            })))
            .mount(&server)
            .await;

        let err = client
            .comment("FEDORA-2026-94cb04410a", "", 1, &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 400"), "{err}");
        assert!(err.contains("your own updates"), "{err}");
    }

    #[test]
    fn with_token_rejects_plaintext_remote_url() {
        let err = BodhiClient::with_base_url("http://bodhi.example.com")
            .with_token("tok".to_string())
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("plaintext"), "{err}");
    }

    #[tokio::test]
    async fn comments_for_user_empty() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/comments/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "comments": [],
                "total": 0,
                "page": 1,
                "pages": 0
            })))
            .mount(&server)
            .await;

        let comments = client.comments_for_user("nobody", 1).await.unwrap();
        assert!(comments.is_empty());
    }
}
