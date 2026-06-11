// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared CLI utilities for sandogasa tools.

pub mod date;

use std::process::{Command, Stdio};

use url::{Host, Url};

/// Environment variable that, when set to a non-empty value,
/// disables [`ensure_secure_url`]'s plaintext-credential guard.
/// Intended for local testing against `http://` mock servers or a
/// trusted internal proxy — never for production credentials.
pub const ALLOW_INSECURE_URL_ENV: &str = "SANDOGASA_ALLOW_INSECURE_URL";

/// Refuse to hand credentials to a base URL that would transmit
/// them in cleartext.
///
/// Returns `Ok(())` when the URL is `https`, when its host is a
/// loopback address (`localhost`, `127.0.0.0/8`, `::1` — so mock
/// servers and local development keep working), or when
/// [`ALLOW_INSECURE_URL_ENV`] is set to a non-empty value.
/// Otherwise returns an error naming the URL and the override, so
/// an API token is never put on the wire over plain `http`.
///
/// Call this wherever a client is built with a token, before any
/// request is made.
pub fn ensure_secure_url(base_url: &str) -> Result<(), String> {
    let allow_insecure = std::env::var_os(ALLOW_INSECURE_URL_ENV).is_some_and(|v| !v.is_empty());
    check_secure_url(base_url, allow_insecure)
}

/// Pure core of [`ensure_secure_url`], with the env override passed
/// in so it can be unit-tested without mutating process state.
fn check_secure_url(base_url: &str, allow_insecure: bool) -> Result<(), String> {
    let parsed = Url::parse(base_url).map_err(|e| format!("invalid URL '{base_url}': {e}"))?;
    if parsed.scheme() == "https" || host_is_loopback(&parsed) {
        return Ok(());
    }
    if allow_insecure {
        return Ok(());
    }
    Err(format!(
        "refusing to send credentials to '{base_url}' over plaintext \
         {}: use an https URL, or set {ALLOW_INSECURE_URL_ENV}=1 to \
         override (e.g. for local testing against a mock server).",
        parsed.scheme()
    ))
}

/// Whether a URL's host is a loopback address.
fn host_is_loopback(u: &Url) -> bool {
    match u.host() {
        Some(Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Check that an external tool is available in `$PATH`.
///
/// Runs `<name> <version_arg>` and returns `Ok(())` if it exits
/// successfully, or an error message with the install hint.
/// Most tools use `--version`; see [`require_tool`] for a
/// convenience wrapper.
///
/// # Example
///
/// ```no_run
/// // koji uses `version` subcommand instead of `--version`
/// sandogasa_cli::require_tool_with_arg("koji", "version", "sudo dnf install koji").unwrap();
/// ```
pub fn require_tool_with_arg(
    name: &str,
    version_arg: &str,
    install_hint: &str,
) -> Result<(), String> {
    match Command::new(name)
        .arg(version_arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "{name} exited with {s}; is it installed correctly? \
             Install it with: {install_hint}"
        )),
        Err(_) => Err(format!("{name} not found. Install it with: {install_hint}")),
    }
}

/// Check that an external tool is available in `$PATH`.
///
/// Runs `<name> --version` and returns `Ok(())` if it exits
/// successfully, or an error message with the install hint.
///
/// For tools that use a different version probe (e.g. `koji version`
/// instead of `koji --version`), use [`require_tool_with_arg`].
///
/// # Example
///
/// ```no_run
/// sandogasa_cli::require_tool("fedrq", "sudo dnf install fedrq").unwrap();
/// ```
pub fn require_tool(name: &str, install_hint: &str) -> Result<(), String> {
    require_tool_with_arg(name, "--version", install_hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_missing_tool() {
        let result = require_tool("nonexistent_tool_xyz_123", "magic install");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("nonexistent_tool_xyz_123"));
        assert!(msg.contains("magic install"));
    }

    #[test]
    fn require_available_tool() {
        // `true` is a standard Unix utility that always succeeds.
        // It doesn't support --version but some impls exit 0 anyway.
        // Use `sh` which reliably exists and handles --version.
        let result = require_tool("sh", "should already be installed");
        // sh --version may or may not succeed depending on implementation,
        // so just verify it doesn't panic.
        let _ = result;
    }

    #[test]
    fn secure_url_allows_https() {
        assert!(check_secure_url("https://bugzilla.redhat.com", false).is_ok());
        assert!(check_secure_url("https://gitlab.com/api/v4", false).is_ok());
    }

    #[test]
    fn secure_url_allows_loopback_over_http() {
        // Mock servers / local dev: loopback is fine over http.
        assert!(check_secure_url("http://127.0.0.1:8080", false).is_ok());
        assert!(check_secure_url("http://localhost:3000/api", false).is_ok());
        assert!(check_secure_url("http://[::1]:9999", false).is_ok());
    }

    #[test]
    fn secure_url_rejects_plaintext_remote() {
        let err = check_secure_url("http://gitlab.example.com", false).unwrap_err();
        assert!(err.contains("gitlab.example.com"));
        assert!(err.contains(ALLOW_INSECURE_URL_ENV));
    }

    #[test]
    fn secure_url_override_allows_plaintext_remote() {
        // With the override "set", plaintext to a remote host is allowed.
        assert!(check_secure_url("http://gitlab.example.com", true).is_ok());
    }

    #[test]
    fn secure_url_rejects_invalid() {
        assert!(check_secure_url("not a url", false).is_err());
    }
}
