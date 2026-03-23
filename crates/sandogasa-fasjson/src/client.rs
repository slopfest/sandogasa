// SPDX-License-Identifier: MPL-2.0

use std::process::Command;

use crate::models::{FasUser, FasjsonResponse};

const FASJSON_BASE: &str = "https://fasjson.fedoraproject.org";

pub struct FasjsonClient {
    base_url: String,
}

impl FasjsonClient {
    pub fn new() -> Self {
        Self {
            base_url: FASJSON_BASE.to_string(),
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
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
            .args(["--negotiate", "-u", ":", "-sf", &url])
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
    fn error_display() {
        let e = FasjsonError::Auth("no ticket".to_string());
        assert_eq!(format!("{e}"), "no ticket");
    }
}
