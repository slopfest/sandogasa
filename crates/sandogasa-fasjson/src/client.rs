// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::Command;

use crate::models::{FasUser, FasjsonPage, FasjsonResponse};

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
    pub fn user(&self, username: &str) -> Result<FasUser, Box<dyn std::error::Error>> {
        let url = format!("{}/v1/users/{}/", self.base_url, username);
        let resp: FasjsonResponse<FasUser> = self.get_json(&url)?;
        Ok(resp.result)
    }

    /// Every member of the FAS group `group`, walking FASJSON's pages.
    /// Membership is what FAS says today; sponsors are members too.
    /// With `max`, a group FAS counts as larger is refused after the
    /// first page, before the rest is fetched — the caller's cap on
    /// how many people it means to look up.
    pub fn group_members(
        &self,
        group: &str,
        max: Option<usize>,
    ) -> Result<Vec<FasUser>, Box<dyn std::error::Error>> {
        let mut members = Vec::new();
        let mut page_number = 1;
        loop {
            let url = format!(
                "{}/v1/groups/{}/members/?page_size=100&page_number={page_number}",
                self.base_url, group
            );
            let page: FasjsonPage<FasUser> = self.get_json(&url)?;
            if let (Some(max), Some(p)) = (max, &page.page)
                && p.total_results as usize > max
            {
                return Err(format!(
                    "{group} has {} members and the cap is {max}; raise --max-members if you mean \
                     to look up every one of them",
                    p.total_results
                )
                .into());
            }
            members.extend(page.result);
            match page.page {
                Some(p) if p.page_number < p.total_pages => page_number = p.page_number + 1,
                _ => break,
            }
        }
        Ok(members)
    }

    /// GET a FASJSON URL through curl (Kerberos rides on `--negotiate`)
    /// and parse the JSON.
    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let output = Command::new("curl")
            .args(curl_args(url))
            .output()
            .map_err(|e| format!("failed to run curl: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("401") || stderr.contains("403") {
                return Err("Kerberos authentication failed — do you have a valid ticket?".into());
            }
            return Err(format!("curl failed (exit {}): {}", output.status, stderr.trim()).into());
        }

        serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("failed to parse FASJSON response: {e}").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_members_page_parses_with_its_paging_block() {
        let json = r#"{"result": [{"username": "decathorpe"}, {"username": "zbyszek"}],
            "page": {"total_results": 10, "page_size": 2, "page_number": 1, "total_pages": 5,
                     "next_page": "https://fasjson.fedoraproject.org/v1/groups/rust-sig/members/?page_size=2&page_number=2"}}"#;
        let page: FasjsonPage<FasUser> = serde_json::from_str(json).unwrap();
        assert_eq!(page.result.len(), 2);
        assert_eq!(page.result[1].username, "zbyszek");
        let p = page.page.unwrap();
        assert_eq!((p.page_number, p.total_pages, p.total_results), (1, 5, 10));
    }

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
    fn user_with_invalid_curl_returns_curl_error() {
        // Use a base_url that curl can't reach to test error path
        let client = FasjsonClient::with_base_url("http://127.0.0.1:1");
        let result = client.user("test");
        assert!(result.is_err());
    }
}
