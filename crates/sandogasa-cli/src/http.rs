// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared HTTP plumbing for sandogasa's API clients (feature `http`).
//!
//! Every client crate used to re-declare the same timeout constant,
//! client-builder dance, and five-line status-check block; they now
//! start from [`builder`] / [`blocking_builder`] and funnel responses
//! through [`ok`] / [`json_ok`] so error formatting is uniform.
//!
//! A service that answers with a bot check (a captcha or a JavaScript
//! challenge page) instead of its API is refusing this network, not
//! failing: [`ok`] says so in one line, without the page, and records
//! the host, so a client asking [`refused`] before each request sends
//! it no more for the rest of the run.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use serde::de::DeserializeOwned;

/// Default timeout for API requests. Generous because some endpoints
/// (large query pages, slow mirrors) legitimately take a while.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// An async client builder preconfigured with the sandogasa
/// defaults: crypto provider installed (see
/// [`crate::install_crypto_provider`]), `user_agent` — typically
/// `concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"))`
/// — and [`TIMEOUT`]. Callers add headers as needed and `.build()`.
pub fn builder(user_agent: &str) -> reqwest::ClientBuilder {
    crate::install_crypto_provider();
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(TIMEOUT)
}

/// Blocking variant of [`builder`].
pub fn blocking_builder(user_agent: &str) -> reqwest::blocking::ClientBuilder {
    crate::install_crypto_provider();
    reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .timeout(TIMEOUT)
}

/// What a refusal's error says, to tell one apart from other errors.
pub const REFUSAL: &str = "refused automated requests from this network";

/// The hosts that answered with a bot check in this run.
static REFUSED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Whether a failed response is a bot check rather than the API's own
/// error: a 403, 429 or 503 with an HTML page that says so, as
/// Repology's "Bot access denied", Anubis's "not a bot", Cloudflare's
/// challenge and PerimeterX's "Access to this page has been denied"
/// pages do.
pub fn is_bot_check(status: u16, content_type: &str, body: &str) -> bool {
    const MARKERS: &[&str] = &[
        "bot access denied",
        "not a bot",
        "are you a robot",
        "captcha",
        "access to this page has been denied",
        "just a moment...",
        "cf-challenge",
    ];
    if !matches!(status, 403 | 429 | 503) {
        return false;
    }
    let body = body.to_lowercase();
    (content_type.contains("html") || body.trim_start().starts_with('<'))
        && MARKERS.iter().any(|m| body.contains(m))
}

/// The host of `url`: what a refusal is recorded under.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_lowercase()
}

/// Record that the host of `url` refused this network, and the error
/// saying so.
fn refuse(url: &str) -> String {
    let host = host_of(url);
    REFUSED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(host.clone());
    refusal_error(&host)
}

fn refusal_error(host: &str) -> String {
    format!("{host} {REFUSAL} (a bot check, not an API error); not asking it again in this run")
}

/// The error to give instead of sending a request to `url`, when its
/// host already refused this network in this run; `None` to send it.
pub fn refused(url: &str) -> Option<String> {
    let host = host_of(url);
    REFUSED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&host)
        .then(|| refusal_error(&host))
}

/// Whether `error` is a refusal ([`REFUSAL`]), for a caller that would
/// rather carry on without the service than fail.
pub fn is_refusal(error: &str) -> bool {
    error.contains(REFUSAL)
}

/// The error for a failed response: a refusal for a bot check, else
/// `what` with the status and response body.
fn failure(
    status: reqwest::StatusCode,
    url: &str,
    content_type: &str,
    text: &str,
    what: &str,
) -> String {
    match is_bot_check(status.as_u16(), content_type, text) {
        true => refuse(url),
        false => format!("{what}: HTTP {status}: {text}"),
    }
}

fn content_type(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Pass a successful response through; a non-success status becomes
/// an error naming `what` (typically the request, e.g. `GET <url>`)
/// with the status and response body, or, for a bot check, a refusal
/// (see the module docs).
pub async fn ok(resp: reqwest::Response, what: &str) -> Result<reqwest::Response, String> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let url = resp.url().to_string();
    let kind = content_type(resp.headers());
    let text = resp.text().await.unwrap_or_default();
    Err(failure(status, &url, &kind, &text, what))
}

/// Deserialize a successful response as JSON; a non-success status or
/// a decode failure becomes an error naming `what`.
pub async fn json_ok<T: DeserializeOwned>(
    resp: reqwest::Response,
    what: &str,
) -> Result<T, String> {
    ok(resp, what)
        .await?
        .json()
        .await
        .map_err(|e| format!("{what}: {e}"))
}

/// Blocking variant of [`ok`].
pub fn blocking_ok(
    resp: reqwest::blocking::Response,
    what: &str,
) -> Result<reqwest::blocking::Response, String> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let url = resp.url().to_string();
    let kind = content_type(resp.headers());
    let text = resp.text().unwrap_or_default();
    Err(failure(status, &url, &kind, &text, what))
}

/// Blocking variant of [`json_ok`].
pub fn blocking_json_ok<T: DeserializeOwned>(
    resp: reqwest::blocking::Response,
    what: &str,
) -> Result<T, String> {
    blocking_ok(resp, what)?
        .json()
        .map_err(|e| format!("{what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_check_page_is_told_from_an_api_error() {
        let repology = "<!DOCTYPE html><html><head><title>Bot access denied</title></head>\
                        <body><h1>Bot access denied</h1><p>We suspect you are a bot.</p>";
        assert!(is_bot_check(403, "text/html; charset=utf-8", repology));
        assert!(is_bot_check(503, "", "<html>Just a moment...</html>"));
        assert!(is_bot_check(
            429,
            "text/html",
            "<p>Making sure you're not a bot!</p>"
        ));
        // An API's own error, even a 403, is not one.
        assert!(!is_bot_check(
            403,
            "application/json",
            r#"{"message": "403 Forbidden"}"#
        ));
        assert!(!is_bot_check(404, "text/html", "<html>captcha</html>"));
        assert!(!is_bot_check(403, "text/html", "<html>Forbidden</html>"));
    }

    #[tokio::test]
    async fn the_async_helpers_report_alike() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/json"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(r#"{"a": 1}"#))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::path("/gone"))
            .respond_with(wiremock::ResponseTemplate::new(404).set_body_string("no"))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::path("/challenge"))
            .respond_with(
                wiremock::ResponseTemplate::new(503)
                    .insert_header("content-type", "text/html")
                    .set_body_string("<html>Just a moment...</html>"),
            )
            .mount(&server)
            .await;
        let http = builder("test/1").build().unwrap();
        let get = |p: &str| http.get(format!("{}{p}", server.uri())).send();
        let v: serde_json::Value = json_ok(get("/json").await.unwrap(), "GET /json")
            .await
            .unwrap();
        assert_eq!(v["a"], 1);
        let err = json_ok::<serde_json::Value>(get("/gone").await.unwrap(), "GET /gone")
            .await
            .unwrap_err();
        assert_eq!(err, "GET /gone: HTTP 404 Not Found: no");
        let err = ok(get("/challenge").await.unwrap(), "GET /challenge")
            .await
            .unwrap_err();
        assert!(is_refusal(&err), "{err}");
        assert!(refused(&server.uri()).is_some());
    }

    #[test]
    fn a_refused_host_is_not_asked_again() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/v1/project/ethtool")
            .with_status(403)
            .with_header("content-type", "text/html")
            .with_body("<html><title>Bot access denied</title></html>")
            .create();
        server
            .mock("GET", "/other")
            .with_status(500)
            .with_body("oops")
            .create();
        let url = format!("{}/api/v1/project/ethtool", server.url());
        assert_eq!(refused(&url), None);
        let http = blocking_builder("test/1").build().unwrap();
        let err = blocking_ok(http.get(&url).send().unwrap(), "GET").unwrap_err();
        assert!(is_refusal(&err), "{err}");
        assert!(!err.contains("<html>"), "{err}");
        // Any path on the host is refused from now on; another error is
        // reported as it was.
        let other = format!("{}/other", server.url());
        assert!(refused(&other).is_some_and(|e| is_refusal(&e)));
        assert!(refused("https://elsewhere.example/x").is_none());
        let err = blocking_ok(http.get(&other).send().unwrap(), "GET /other").unwrap_err();
        assert_eq!(err, "GET /other: HTTP 500 Internal Server Error: oops");
        assert_eq!(
            host_of("HTTPS://Repology.org:443/api?x"),
            "repology.org:443"
        );
    }
}
