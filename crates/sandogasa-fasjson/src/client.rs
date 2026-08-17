// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::Command;

use crate::models::{FasUser, FasjsonResponse};

const FASJSON_BASE: &str = "https://fasjson.fedoraproject.org";

/// What this crate calls itself to a server, matching the string the
/// reqwest-based crates here send through `sandogasa_cli::http`.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// How this crate invokes curl.
///
/// Separate from the call so the arguments can be asserted: the
/// user-agent in particular is invisible when it works and costs a
/// tarpitted request when it is missing, which is not a thing to leave to
/// inspection.
///
/// `--` so the URL can never be read as a curl option. `--max-time` so a
/// hung connection cannot block forever, at the same 120s bound the
/// reqwest-based sibling crates use. `--user-agent` because Fedora's
/// infrastructure tarpits requests that arrive without one, and inheriting
/// curl's default would leave this crate the only client here that a
/// server log cannot identify.
fn curl_args(url: &str) -> Vec<&str> {
    vec![
        "--negotiate",
        "-u",
        ":",
        "-sf",
        "--max-time",
        "120",
        "--user-agent",
        USER_AGENT,
        "--",
        url,
    ]
}

pub struct FasjsonClient {
    base_url: String,
}

impl Default for FasjsonClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FasjsonClient {
    pub fn new() -> Self {
        Self::with_base_url(FASJSON_BASE)
    }

    pub fn with_base_url(base_url: &str) -> Self {
        sandogasa_cli::install_crypto_provider();

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Fetch a user profile from FASJSON using Kerberos negotiate auth.
    ///
    /// Shells out to `curl --negotiate` since FASJSON requires GSSAPI
    /// authentication and there is no pure-Rust GSSAPI implementation
    /// that avoids a build-time dependency on system krb5 libraries.
    pub fn user(&self, username: &str) -> Result<FasUser, FasjsonError> {
        let url = format!("{}/v1/users/{}/", self.base_url, username);
        let output = Command::new("curl")
            .args(curl_args(&url))
            .output()
            .map_err(|e| FasjsonError::Curl(format!("failed to run curl: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("401") || stderr.contains("403") {
                return Err(FasjsonError::Auth(
                    "Kerberos authentication failed — do you have a valid ticket?".to_string(),
                ));
            }
            return Err(FasjsonError::Curl(format!(
                "curl failed (exit {}): {}",
                output.status,
                stderr.trim()
            )));
        }

        let resp: FasjsonResponse<FasUser> = serde_json::from_slice(&output.stdout)
            .map_err(|e| FasjsonError::Parse(format!("failed to parse FASJSON response: {e}")))?;

        Ok(resp.result)
    }
}

#[derive(Debug)]
pub enum FasjsonError {
    /// curl command failed.
    Curl(String),
    /// Kerberos authentication failed.
    Auth(String),
    /// Failed to parse JSON response.
    Parse(String),
}

impl std::fmt::Display for FasjsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FasjsonError::Curl(msg) => write!(f, "{msg}"),
            FasjsonError::Auth(msg) => write!(f, "{msg}"),
            FasjsonError::Parse(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for FasjsonError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_is_told_who_is_calling() {
        // Fedora's infrastructure tarpits requests without a user-agent,
        // and this crate is the one that shells out rather than using
        // reqwest, so nothing else would supply it.
        let args = curl_args("https://example.invalid/v1/users/alice/");
        let at = args
            .iter()
            .position(|a| *a == "--user-agent")
            .expect("no user-agent passed to curl");
        let sent = args[at + 1];
        assert!(sent.starts_with("sandogasa-fasjson/"), "{sent}");
        assert!(
            sent.len() > "sandogasa-fasjson/".len(),
            "no version in {sent}"
        );
        // The URL stays last, behind `--`, so it can never be read as an
        // option however it is spelled.
        assert_eq!(args[args.len() - 2], "--");
        assert_eq!(
            *args.last().unwrap(),
            "https://example.invalid/v1/users/alice/"
        );
    }

    #[test]
    fn new_uses_default_base_url() {
        let client = FasjsonClient::new();
        assert_eq!(client.base_url, "https://fasjson.fedoraproject.org");
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = FasjsonClient::with_base_url("https://fasjson.example.com/");
        assert_eq!(client.base_url, "https://fasjson.example.com");
    }

    #[test]
    fn error_display_auth() {
        let e = FasjsonError::Auth("no ticket".to_string());
        assert_eq!(format!("{e}"), "no ticket");
    }

    #[test]
    fn error_display_curl() {
        let e = FasjsonError::Curl("curl failed".to_string());
        assert_eq!(format!("{e}"), "curl failed");
    }

    #[test]
    fn error_display_parse() {
        let e = FasjsonError::Parse("bad json".to_string());
        assert_eq!(format!("{e}"), "bad json");
    }

    #[test]
    fn error_is_std_error() {
        let e: Box<dyn std::error::Error> = Box::new(FasjsonError::Auth("test".to_string()));
        assert_eq!(format!("{e}"), "test");
    }

    #[test]
    fn user_with_invalid_curl_returns_curl_error() {
        // Use a base_url that curl can't reach to test error path
        let client = FasjsonClient::with_base_url("http://127.0.0.1:1");
        let result = client.user("test");
        assert!(result.is_err());
    }
}
