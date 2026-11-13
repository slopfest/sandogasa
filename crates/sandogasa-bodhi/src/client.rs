// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;
use sandogasa_cli::http;

use crate::models::{
    BodhiRelease, BugFeedbackItem, Comment, CommentsResponse, NewUpdateFromTag, NewUpdateResponse,
    ReleasesResponse, SingleCommentResponse, SingleUpdateResponse, Update, UpdatesResponse,
};

const BODHI_API_BASE: &str = "https://bodhi.fedoraproject.org";

/// The form that edits the update `update` (as `GET /updates/<alias>`
/// answers it, the `update` member) to carry `bugs`, closed on stable
/// or not as `close_bugs` says, with every other field as it is.
///
/// Bodhi edits by re-saving the whole update, so each field the update
/// has a value for is sent back, as bodhi-client's `updates edit` does:
/// a field left out is set to its default. A field Bodhi answers as null
/// is left out, as bodhi-client leaves it out too. A side tag update is
/// re-sent by its tag, and Bodhi refreshes its builds from the tag when
/// it saves, so a caller should check first that the tag holds no build
/// the update does not; any other update is re-sent by its builds.
pub fn edit_bugs_form(
    update: &serde_json::Value,
    bugs: &[u64],
    close_bugs: bool,
) -> Result<Vec<(String, String)>, String> {
    let alias = update["alias"]
        .as_str()
        .ok_or("the update has no alias")?
        .to_string();
    let mut form = vec![("edited".to_string(), alias)];
    for field in [
        "type",
        "severity",
        "notes",
        "display_name",
        "stable_karma",
        "unstable_karma",
        "stable_days",
        "autokarma",
        "autotime",
        "requirements",
        "suggest",
        "require_bugs",
        "require_testcases",
    ] {
        let value = match &update[field] {
            serde_json::Value::Null => continue,
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            other => return Err(format!("the update's {field} is unexpected: {other}")),
        };
        form.push((field.to_string(), value));
    }
    form.push(("close_bugs".to_string(), close_bugs.to_string()));
    form.push((
        "bugs".to_string(),
        bugs.iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(","),
    ));
    match update["from_tag"].as_str().filter(|t| !t.is_empty()) {
        Some(tag) => form.push(("from_tag".to_string(), tag.to_string())),
        None => {
            let builds = update["builds"]
                .as_array()
                .ok_or("the update lists no builds")?;
            for build in builds {
                let nvr = build["nvr"].as_str().ok_or("a build has no NVR")?;
                form.push(("builds".to_string(), nvr.to_string()));
            }
        }
    }
    Ok(form)
}

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
    http::builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
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
        Self::with_base_url(BODHI_API_BASE)
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
        let status_params: String = statuses.iter().map(|s| format!("&status={s}")).collect();

        loop {
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

    /// The updates a build is in, whatever their status: Bodhi lets a
    /// build belong to one update, and refuses a new update (from a
    /// side tag, say) carrying a build another one already has.
    pub async fn updates_for_build(&self, nvr: &str) -> Result<Vec<Update>, reqwest::Error> {
        let mut all_updates = Vec::new();
        let mut page = 1;
        loop {
            let resp: UpdatesResponse = self
                .client
                .get(format!("{}/updates/", self.base_url))
                .query(&[
                    ("builds", nvr),
                    ("rows_per_page", "100"),
                    ("page", &page.to_string()),
                ])
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

    /// The updates any of `nvrs` is in, whatever their status, in one
    /// query (paginated) rather than one per build: Bodhi takes the
    /// `builds` parameter repeated. Each update lists its builds, which
    /// is how a caller matches it back to the NVRs it asked about.
    pub async fn updates_for_builds(&self, nvrs: &[String]) -> Result<Vec<Update>, reqwest::Error> {
        let mut all_updates = Vec::new();
        if nvrs.is_empty() {
            return Ok(all_updates);
        }
        let mut page = 1;
        loop {
            let mut query: Vec<(&str, String)> =
                nvrs.iter().map(|n| ("builds", n.clone())).collect();
            query.push(("rows_per_page", "100".to_string()));
            query.push(("page", page.to_string()));
            let resp: UpdatesResponse = self
                .client
                .get(format!("{}/updates/", self.base_url))
                .query(&query)
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

    /// The updates carrying any of `bug_ids`, whatever their status, in
    /// one query (paginated): Bodhi takes the `bugs` parameter
    /// repeated. Each update lists its bugs, which is how a caller
    /// matches it back to the bugs it asked about.
    pub async fn updates_for_bugs(&self, bug_ids: &[u64]) -> Result<Vec<Update>, reqwest::Error> {
        let mut all_updates = Vec::new();
        if bug_ids.is_empty() {
            return Ok(all_updates);
        }
        let mut page = 1;
        loop {
            let mut query: Vec<(&str, String)> =
                bug_ids.iter().map(|id| ("bugs", id.to_string())).collect();
            query.push(("rows_per_page", "100".to_string()));
            query.push(("page", page.to_string()));
            let resp: UpdatesResponse = self
                .client
                .get(format!("{}/updates/", self.base_url))
                .query(&query)
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
            // EPEL Next is its own platform rather than a staging area:
            // it builds against CentOS Stream, as `epel9-next` does for
            // Stream 9 where `epel10` does for Stream 10, so a caller
            // asking what is active wants it. ELN is excluded instead,
            // taking no reviewed updates though it rides under FEDORA.
            .filter(|r| {
                matches!(
                    r.id_prefix.as_str(),
                    "FEDORA" | "FEDORA-EPEL" | "FEDORA-EPEL-NEXT"
                ) && r.name != "ELN"
            })
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
        // A page carries full update objects — builds, comments, karma —
        // so a caller wanting one row must not be sent a hundred (1.7 MB
        // and ten seconds against 5 KB and under half a second).
        let rows = limit.clamp(1, 100);
        loop {
            let url = format!(
                "{}/updates/?user={}{}&rows_per_page={rows}&chrome=0&page={}",
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

    /// Run the authenticated write sequence shared by
    /// [`Self::comment`] and [`Self::new_update_from_tag`]: login
    /// (session cookie), fetch a CSRF token, append it to `form`,
    /// POST the form to `path`, and decode the JSON response.
    /// Non-2xx responses become errors naming `what` and carrying
    /// the response body (Bodhi's validation errors).
    ///
    /// The POST itself is deliberately not auto-retried: a
    /// transport error after the server processed the request
    /// would repeat a non-idempotent write. The retried
    /// login/csrf steps absorb most transient failures.
    async fn post_form<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        mut form: Vec<(String, String)>,
        what: &str,
    ) -> Result<T, Box<dyn std::error::Error>> {
        self.login().await?;
        form.push(("csrf_token".to_string(), self.csrf().await?));
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .auth(self.client.post(&url))
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&form)
            .send()
            .await?;
        Ok(http::json_ok(resp, what).await?)
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
        let mut form: Vec<(String, String)> = vec![
            ("update".to_string(), update_alias.to_string()),
            ("text".to_string(), text.to_string()),
            ("karma".to_string(), karma.to_string()),
        ];
        for (i, fb) in bug_feedback.iter().enumerate() {
            form.push((format!("bug_feedback.{i}.bug_id"), fb.bug_id.to_string()));
            form.push((format!("bug_feedback.{i}.karma"), fb.karma.to_string()));
        }
        self.post_form("/comments/", form, "posting comment").await
    }

    /// Change which bugs the update `alias` carries, and whether Bodhi
    /// closes them when it goes stable, leaving everything else as it
    /// is: the update is read as Bodhi has it and saved back with only
    /// those two changed (see [`edit_bugs_form`]). Requires a bearer
    /// token ([`Self::with_token`]).
    pub async fn edit_update_bugs(
        &self,
        alias: &str,
        bugs: &[u64],
        close_bugs: bool,
    ) -> Result<NewUpdateResponse, Box<dyn std::error::Error>> {
        let url = format!("{}/updates/{}", self.base_url, alias);
        let answer: serde_json::Value = self
            .client
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let form = edit_bugs_form(&answer["update"], bugs, close_bugs)?;
        self.post_form("/updates/", form, "editing update").await
    }

    /// Create an update from a Koji side tag (`POST /updates/` with
    /// `from_tag`), the API behind `bodhi updates new --from-tag`.
    /// Requires a bearer token ([`Self::with_token`]) whose account
    /// owns the side tag.
    ///
    /// The body is form-encoded like [`Self::comment`]; `bugs` is
    /// sent as one comma-joined value (Bodhi's schema splits it
    /// server-side, matching the web UI). Non-2xx responses are
    /// surfaced with the response body, which carries Bodhi's
    /// validation errors (e.g. unsigned builds, or a tag that isn't
    /// a side tag).
    pub async fn new_update_from_tag(
        &self,
        req: &NewUpdateFromTag,
    ) -> Result<NewUpdateResponse, Box<dyn std::error::Error>> {
        let mut form: Vec<(String, String)> = match &req.source {
            crate::models::UpdateSource::FromTag(tag) => {
                vec![("from_tag".to_string(), tag.clone())]
            }
            // Repeated rather than comma-joined: Bodhi's schema takes
            // `builds` as a list, and a comma-joined value arrives as
            // one build whose name contains commas.
            crate::models::UpdateSource::Builds(nvrs) => nvrs
                .iter()
                .map(|nvr| ("builds".to_string(), nvr.clone()))
                .collect(),
        };
        form.extend([
            ("notes".to_string(), req.notes.clone()),
            ("type".to_string(), req.update_type.clone()),
            ("severity".to_string(), req.severity.clone()),
            ("close_bugs".to_string(), req.close_bugs.to_string()),
            ("autokarma".to_string(), req.autokarma.to_string()),
            ("stable_karma".to_string(), req.stable_karma.to_string()),
            ("unstable_karma".to_string(), req.unstable_karma.to_string()),
        ]);
        if !req.bugs.is_empty() {
            let bugs = req
                .bugs
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            form.push(("bugs".to_string(), bugs));
        }
        self.post_form("/updates/", form, "creating update").await
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
    async fn updates_for_builds_asks_about_all_of_them_at_once() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/updates/"))
            .and(wiremock::matchers::query_param(
                "builds",
                "asterisk-24.0.0-3.fc46",
            ))
            .and(wiremock::matchers::query_param(
                "builds",
                "bluez-5.88-1.fc46",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [{
                    "alias": "FEDORA-2026-0123456789", "status": "stable",
                    "builds": [{"nvr": "asterisk-24.0.0-3.fc46"}],
                }, {
                    "alias": "FEDORA-2026-1ebc714fec", "status": "stable",
                    "builds": [{"nvr": "bluez-5.88-1.fc46"}],
                }],
                "page": 1, "pages": 1, "total": 2,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let nvrs = ["asterisk-24.0.0-3.fc46", "bluez-5.88-1.fc46"].map(String::from);
        let updates = client.updates_for_builds(&nvrs).await.unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[1].builds[0].nvr, "bluez-5.88-1.fc46");
        assert!(client.updates_for_builds(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn updates_for_user_pages_until_the_limit_and_reports_progress() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());
        let page = |n: u64, aliases: &[&str]| {
            serde_json::json!({
                "updates": aliases
                    .iter()
                    .map(|a| serde_json::json!({"alias": a, "status": "stable"}))
                    .collect::<Vec<_>>(),
                "page": n, "pages": 3, "total": 6,
            })
        };
        for (n, aliases) in [
            (1, ["FEDORA-2026-a", "FEDORA-2026-b"]),
            (2, ["FEDORA-2026-c", "FEDORA-2026-d"]),
        ] {
            Mock::given(method("GET"))
                .and(wiremock::matchers::path("/updates/"))
                .and(wiremock::matchers::query_param("user", "salimma"))
                .and(wiremock::matchers::query_param(
                    "submitted_since",
                    "2026-10-01",
                ))
                .and(wiremock::matchers::query_param(
                    "submitted_before",
                    "2026-11-01",
                ))
                .and(wiremock::matchers::query_param("rows_per_page", "3"))
                .and(wiremock::matchers::query_param("page", n.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(page(n, &aliases)))
                .expect(1)
                .mount(&server)
                .await;
        }
        let mut seen = Vec::new();
        let updates = client
            .updates_for_user(
                "salimma",
                3,
                chrono::NaiveDate::from_ymd_opt(2026, 10, 1),
                chrono::NaiveDate::from_ymd_opt(2026, 11, 1),
                |page, pages, count| seen.push((page, pages, count)),
            )
            .await
            .unwrap();
        // The third page is never asked for, and the extra row is cut.
        let aliases: Vec<&str> = updates.iter().map(|u| u.alias.as_str()).collect();
        assert_eq!(aliases, ["FEDORA-2026-a", "FEDORA-2026-b", "FEDORA-2026-c"]);
        assert_eq!(seen, [(1, 3, 2), (2, 3, 4)]);
    }

    #[tokio::test]
    async fn comments_for_user_asks_for_the_limit() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/comments/"))
            .and(wiremock::matchers::query_param("user", "salimma"))
            .and(wiremock::matchers::query_param("rows_per_page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "comments": [
                    {"id": 2, "text": "Works for me", "karma": 1},
                    {"id": 1, "text": "Still broken", "karma": -1},
                ],
                "page": 1, "pages": 1, "total": 2,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let comments = client.comments_for_user("salimma", 2).await.unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].karma, -1);
        // A failing Bodhi is an error, not an empty list.
        let down = BodhiClient::with_base_url("http://127.0.0.1:9");
        assert!(down.comments_for_user("salimma", 2).await.is_err());
    }

    #[tokio::test]
    async fn updates_for_bugs_asks_about_all_of_them_at_once() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/updates/"))
            .and(wiremock::matchers::query_param("bugs", "2569815"))
            .and(wiremock::matchers::query_param("bugs", "2572044"))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [{
                    "alias": "FEDORA-EPEL-2026-0123456789", "status": "testing",
                    "builds": [{"nvr": "sandogasa-0.28.0-1.el10_4"}],
                    "bugs": [{"bug_id": 2569815}],
                }],
                "page": 1, "pages": 2, "total": 2,
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/updates/"))
            .and(wiremock::matchers::query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [{
                    "alias": "FEDORA-2026-1ebc714fec", "status": "stable",
                    "builds": [{"nvr": "sandogasa-0.28.0-1.fc46"}],
                    "bugs": [{"bug_id": 2569815}],
                }],
                "page": 2, "pages": 2, "total": 2,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let updates = client.updates_for_bugs(&[2569815, 2572044]).await.unwrap();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].bugs[0].bug_id, 2569815);
        assert_eq!(updates[1].alias, "FEDORA-2026-1ebc714fec");
        assert!(client.updates_for_bugs(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn updates_for_build_asks_by_nvr() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(wiremock::matchers::path("/updates/"))
            .and(wiremock::matchers::query_param(
                "builds",
                "asterisk-24.0.0-3.fc46",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [{
                    "alias": "FEDORA-2026-0123456789", "status": "stable",
                    "builds": [{"nvr": "asterisk-24.0.0-3.fc46"}],
                }],
                "total": 1,
                "page": 1, "pages": 1,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let updates = client
            .updates_for_build("asterisk-24.0.0-3.fc46")
            .await
            .unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].alias, "FEDORA-2026-0123456789");
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
                    {"name": "EPEL-9N", "branch": "epel9-next", "id_prefix": "FEDORA-EPEL-NEXT",
                     "state": "current"},
                    {"name": "F45", "branch": "f45", "id_prefix": "FEDORA", "state": "frozen"},
                    {"name": "F42F", "branch": "f42f", "id_prefix": "FEDORA-FLATPAK", "state": "current"},
                    {"name": "F40", "branch": "f40", "id_prefix": "FEDORA", "state": "archived"}
                ],
                "total": 7,
                "page": 1,
                "pages": 1
            })))
            .mount(&server)
            .await;

        let releases = client.active_releases().await.unwrap();
        let names: Vec<_> = releases.iter().map(|r| r.name.as_str()).collect();
        // EPEL Next is active; ELN, Flatpak and an archived release are
        // not. A frozen release is still active -- F45 is frozen while
        // it is branched, and rawhide is pending, so neither state is
        // the plain "current" one might expect to filter on.
        assert_eq!(names, vec!["F43", "EPEL-9", "EPEL-9N", "F45"]);
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
    async fn updates_for_user_asks_for_only_the_rows_wanted() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());
        // A last-seen check wants one update; a page of a hundred full
        // update objects is 1.7 MB and ten seconds for nothing.
        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .and(wiremock::matchers::query_param("rows_per_page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [],
                "total": 0,
                "page": 1,
                "pages": 0
            })))
            .expect(1)
            .mount(&server)
            .await;
        client
            .updates_for_user("nobody", 1, None, None, |_, _, _| {})
            .await
            .unwrap();
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

    #[test]
    fn the_default_client_is_fedora_s_bodhi() {
        assert_eq!(BodhiClient::default().base_url, BODHI_API_BASE);
    }

    #[tokio::test]
    async fn each_query_reads_every_page() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());
        for n in [1u64, 2] {
            Mock::given(method("GET"))
                .and(wiremock::matchers::path("/updates/"))
                .and(wiremock::matchers::query_param("page", n.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "updates": [{"alias": format!("FEDORA-2026-{n}"), "status": "stable"}],
                    "page": n, "pages": 2, "total": 2,
                })))
                .mount(&server)
                .await;
        }
        let two = |updates: Vec<Update>| updates.into_iter().map(|u| u.alias).collect::<Vec<_>>();
        let want = ["FEDORA-2026-1", "FEDORA-2026-2"];
        assert_eq!(
            two(client
                .updates_for_package("sandogasa", "F46", &["testing"])
                .await
                .unwrap()),
            want
        );
        assert_eq!(
            two(client
                .updates_for_build("sandogasa-0.28.1-1.fc46")
                .await
                .unwrap()),
            want
        );
        let nvrs = ["sandogasa-0.28.1-1.fc46".to_string()];
        assert_eq!(two(client.updates_for_builds(&nvrs).await.unwrap()), want);
    }

    #[tokio::test]
    async fn a_refused_login_says_to_log_in_again() {
        use wiremock::matchers::path;

        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri())
            .with_token("tok-expired".to_string())
            .unwrap();
        Mock::given(method("GET"))
            .and(path("/oidc/login-token"))
            .respond_with(ResponseTemplate::new(401).set_body_string("token expired"))
            .mount(&server)
            .await;
        // Nothing is posted without a session.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let err = client
            .comment("FEDORA-2026-94cb04410a", "", 1, &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("bodhi login failed (HTTP 401"), "{err}");
        assert!(err.contains("token expired"), "{err}");
        assert!(err.contains("re-authenticate with the bodhi CLI"), "{err}");
    }

    #[tokio::test]
    async fn new_update_from_builds_names_each_build() {
        use wiremock::matchers::{body_string_contains, path};

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
            .and(path("/updates/"))
            .and(body_string_contains("builds=sandogasa-0.28.1-2.el10_4"))
            .and(body_string_contains("builds=rust-uucore-0.2.0-1.el10_4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "alias": "FEDORA-EPEL-2026-0281000000",
                "status": "pending",
                "builds": [
                    {"nvr": "sandogasa-0.28.1-2.el10_4"},
                    {"nvr": "rust-uucore-0.2.0-1.el10_4"}
                ],
            })))
            .expect(1)
            .mount(&server)
            .await;
        let req = crate::models::NewUpdateFromTag {
            source: crate::models::UpdateSource::Builds(vec![
                "sandogasa-0.28.1-2.el10_4".to_string(),
                "rust-uucore-0.2.0-1.el10_4".to_string(),
            ]),
            ..from_tag_request()
        };
        let resp = client.new_update_from_tag(&req).await.unwrap();
        assert_eq!(resp.aliases(), ["FEDORA-EPEL-2026-0281000000"]);
    }

    fn from_tag_request() -> crate::models::NewUpdateFromTag {
        crate::models::NewUpdateFromTag {
            source: crate::models::UpdateSource::FromTag("epel9-build-side-133287".to_string()),
            notes: "Update uutils to 0.2".to_string(),
            update_type: "enhancement".to_string(),
            severity: "unspecified".to_string(),
            bugs: vec![100, 200],
            close_bugs: true,
            autokarma: true,
            stable_karma: 3,
            unstable_karma: -3,
        }
    }

    #[tokio::test]
    async fn new_update_from_tag_posts_form_and_parses_single_shape() {
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
            .and(path("/updates/"))
            .and(header("authorization", "Bearer tok-123"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("from_tag=epel9-build-side-133287"))
            .and(body_string_contains("notes=Update+uutils+to+0.2"))
            .and(body_string_contains("type=enhancement"))
            .and(body_string_contains("severity=unspecified"))
            .and(body_string_contains("close_bugs=true"))
            .and(body_string_contains("autokarma=true"))
            .and(body_string_contains("stable_karma=3"))
            .and(body_string_contains("unstable_karma=-3"))
            // Comma-joined (URL-encoded), split server-side.
            .and(body_string_contains("bugs=100%2C200"))
            .and(body_string_contains("csrf_token=csrf-abc"))
            // The from_tag single-update response: update fields at
            // the top level, caveats alongside.
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "alias": "FEDORA-EPEL-2026-f9eaa11e18",
                "status": "pending",
                "from_tag": "epel9-build-side-133287",
                "builds": [{"nvr": "rust-uucore-0.2.0-1.el9"}],
                "caveats": [
                    {"description": "Your update is currently being composed."}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let resp = client
            .new_update_from_tag(&from_tag_request())
            .await
            .unwrap();
        assert_eq!(resp.aliases(), ["FEDORA-EPEL-2026-f9eaa11e18"]);
        assert_eq!(resp.caveats.len(), 1);
    }

    fn an_update(from_tag: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "alias": "FEDORA-2026-0b2e1b3c7d",
            "type": "enhancement",
            "severity": "unspecified",
            "notes": "libical 4.x and reverse dependencies",
            "display_name": "",
            "stable_karma": 1,
            "unstable_karma": -3,
            "stable_days": 0,
            "autokarma": true,
            "autotime": true,
            "close_bugs": false,
            "requirements": null,
            "suggest": "unspecified",
            "require_bugs": false,
            "require_testcases": false,
            "from_tag": from_tag,
            "bugs": [{"bug_id": 2458837}, {"bug_id": 2567306}],
            "builds": [{"nvr": "libical-4.0.7-1.fc46"}, {"nvr": "asterisk-24.0.0-3.fc46.1"}]
        })
    }

    #[test]
    fn an_edit_sends_every_field_back_with_only_the_bugs_changed() {
        let form =
            edit_bugs_form(&an_update(Some("f46-build-side-158330")), &[2458837], true).unwrap();
        let get = |k: &str| {
            form.iter()
                .filter(|(f, _)| f == k)
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(get("edited"), ["FEDORA-2026-0b2e1b3c7d"]);
        assert_eq!(get("bugs"), ["2458837"]);
        assert_eq!(get("close_bugs"), ["true"]);
        // Everything else as the update has it.
        assert_eq!(get("type"), ["enhancement"]);
        assert_eq!(get("notes"), ["libical 4.x and reverse dependencies"]);
        assert_eq!(get("stable_karma"), ["1"]);
        assert_eq!(get("unstable_karma"), ["-3"]);
        assert_eq!(get("autokarma"), ["true"]);
        assert_eq!(get("autotime"), ["true"]);
        assert_eq!(get("suggest"), ["unspecified"]);
        // A side tag update is re-sent by its tag, not its builds.
        assert_eq!(get("from_tag"), ["f46-build-side-158330"]);
        assert!(get("builds").is_empty());
        // A null field is left out, as bodhi-client leaves it out.
        assert!(get("requirements").is_empty());

        // An update of builds is re-sent build by build, and an empty
        // bug list clears them.
        let form = edit_bugs_form(&an_update(None), &[], false).unwrap();
        let builds: Vec<&str> = form
            .iter()
            .filter(|(f, _)| f == "builds")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(builds, ["libical-4.0.7-1.fc46", "asterisk-24.0.0-3.fc46.1"]);
        assert!(form.contains(&("bugs".to_string(), String::new())));
        assert!(edit_bugs_form(&serde_json::json!({}), &[], false).is_err());
    }

    #[tokio::test]
    async fn edit_update_bugs_reads_the_update_and_saves_it_back() {
        use wiremock::matchers::{body_string_contains, header, path};

        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri())
            .with_token("tok-123".to_string())
            .unwrap();
        Mock::given(method("GET"))
            .and(path("/updates/FEDORA-2026-0b2e1b3c7d"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "update": an_update(Some("f46-build-side-158330"))
            })))
            .expect(1)
            .mount(&server)
            .await;
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
            .and(path("/updates/"))
            .and(header("authorization", "Bearer tok-123"))
            .and(body_string_contains("edited=FEDORA-2026-0b2e1b3c7d"))
            .and(body_string_contains("bugs=2458837&"))
            .and(body_string_contains("from_tag=f46-build-side-158330"))
            .and(body_string_contains("csrf_token=csrf-abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "alias": "FEDORA-2026-0b2e1b3c7d",
                "caveats": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let resp = client
            .edit_update_bugs("FEDORA-2026-0b2e1b3c7d", &[2458837], false)
            .await
            .unwrap();
        assert_eq!(resp.aliases(), ["FEDORA-2026-0b2e1b3c7d"]);
    }

    #[tokio::test]
    async fn new_update_from_tag_parses_multi_update_shape_and_errors() {
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
            .and(path("/updates/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [
                    {"alias": "FEDORA-2026-aaa", "status": "pending"},
                    {"alias": "FEDORA-2026-bbb", "status": "pending"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let resp = client
            .new_update_from_tag(&from_tag_request())
            .await
            .unwrap();
        assert_eq!(resp.aliases(), ["FEDORA-2026-aaa", "FEDORA-2026-bbb"]);
        assert!(resp.caveats.is_empty());

        // A validation failure surfaces Bodhi's error body.
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
            .and(path("/updates/"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                r#"{"errors": [{"description": "Cannot find any tag with name: not-a-tag"}]}"#,
            ))
            .mount(&server)
            .await;
        let err = client
            .new_update_from_tag(&from_tag_request())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 400"), "{err}");
        assert!(err.contains("Cannot find any tag"), "{err}");
    }
}
