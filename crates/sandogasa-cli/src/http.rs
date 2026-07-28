// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared HTTP plumbing for sandogasa's API clients (feature `http`).
//!
//! Every client crate used to re-declare the same timeout constant,
//! client-builder dance, and five-line status-check block; they now
//! start from [`builder`] / [`blocking_builder`] and funnel responses
//! through [`ok`] / [`json_ok`] so error formatting is uniform.

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

/// Pass a successful response through; a non-success status becomes
/// an error naming `what` (typically the request, e.g. `GET <url>`)
/// with the status and response body.
pub async fn ok(resp: reqwest::Response, what: &str) -> Result<reqwest::Response, String> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    Err(format!("{what}: HTTP {status}: {text}"))
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
    let text = resp.text().unwrap_or_default();
    Err(format!("{what}: HTTP {status}: {text}"))
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
