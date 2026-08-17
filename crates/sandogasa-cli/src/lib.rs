// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared CLI utilities for sandogasa tools.

pub mod claim;
pub mod date;
pub mod defaults;
#[cfg(feature = "http")]
pub mod http;
#[cfg(feature = "man")]
pub mod man;

pub use defaults::parse_with_defaults;

use std::process::{Command, Stdio};

use url::{Host, Url};

/// Standard process-wide initialization for sandogasa tools.
///
/// Call this once as the first statement of `main()` in every
/// binary. It is the single place for cross-cutting startup work:
/// anything added to this function is automatically picked up by
/// every tool that calls it, so prefer extending `init` over
/// scattering setup across mains.
///
/// Today it registers the rustls crypto provider that reqwest's
/// TLS support needs (see [`install_crypto_provider`]). Idempotent
/// and cheap, so calling it from a tool that does no networking is
/// harmless.
pub fn init() {
    install_crypto_provider();
}

/// Install the ring-based rustls [`CryptoProvider`] as the process
/// default.
///
/// We build reqwest with the `rustls-no-provider` feature to keep
/// `aws-lc-rs` — reqwest 0.13's default provider, which is not
/// packaged in Fedora — out of the dependency tree. That leaves
/// rustls with no compiled-in default provider, so one must be
/// registered at runtime before the first HTTPS request or reqwest
/// panics with "No provider set". `ring` is statically linked into
/// the binary (a build-time dependency only); this just points
/// rustls at it.
///
/// Idempotent: the underlying `install_default` only takes effect
/// on the first call and reports an error on subsequent ones, which
/// we ignore so repeated calls (e.g. across tests) are harmless.
///
/// [`CryptoProvider`]: rustls::crypto::CryptoProvider
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

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

/// Whether an executable named `name` is on `$PATH` (a lightweight
/// check that does **not** run the tool).
pub fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Whether `exe` is available, per its `probe`: `Some(arg)` runs
/// `exe arg` and requires a zero exit (confirms it executes);
/// `None` checks only `$PATH` existence.
fn tool_available(exe: &str, probe: Option<&str>) -> bool {
    match probe {
        Some(arg) => Command::new(exe)
            .arg(arg)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
        None => tool_exists(exe),
    }
}

/// Check that a batch of external tools is available, returning a
/// single error that lists every missing one with its install hint.
///
/// Each entry is `(executable, install_hint, probe)`:
/// - `probe = Some(arg)` *runs* `<executable> <arg>` (e.g.
///   `Some("--version")`, or `Some("version")` for `koji`, or
///   `Some("--help")` for `pbuilder-dist`) and requires a zero exit,
///   confirming the tool actually executes.
/// - `probe = None` checks only `$PATH` existence, for tools with no
///   usable version/help flag.
///
/// All entries are checked, so the error names every missing tool
/// rather than failing on the first.
///
/// # Example
///
/// ```no_run
/// sandogasa_cli::require_tools(&[
///     ("git", "sudo apt install git", Some("--version")),
///     ("pbuilder-dist", "sudo apt install ubuntu-dev-tools", Some("--help")),
/// ])
/// .unwrap();
/// ```
pub fn require_tools(tools: &[(&str, &str, Option<&str>)]) -> Result<(), String> {
    let missing: Vec<String> = tools
        .iter()
        .filter(|(exe, _, probe)| !tool_available(exe, *probe))
        .map(|(exe, hint, _)| format!("{exe} (install: {hint})"))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("missing required tool(s): {}", missing.join(", ")))
    }
}

/// Word-wrap `text` to `width` columns and prefix every line with
/// `prefix` (e.g. `"> "` for a Markdown blockquote, or the leading
/// indent of a wrapped list item). Collapses runs of whitespace and
/// never splits a word, so a single token longer than the width — a
/// URL, typically — overflows rather than being broken. Such a token
/// also stays on the line it started on: breaking before a word that
/// won't fit on a fresh line either would only orphan whatever label
/// introduces it (`LINK:`, `Minutes:`) while still overflowing.
pub fn wrap_prefixed(text: &str, prefix: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty()
            && prefix.len() + line.len() + 1 + word.len() > width
            && prefix.len() + word.len() <= width
        {
            out.push_str(prefix);
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(prefix);
        out.push_str(&line);
    }
    out
}

/// Ask a yes/no question on stderr (keeping stdout clean for piped
/// or `--json` output) and read one line from stdin.
///
/// `y`/`yes` and `n`/`no` (any case) answer explicitly; anything
/// else — including just Enter or EOF — takes the default. The
/// prompt shows `[Y/n]` or `[y/N]` to match `default_yes`. Callers
/// must not prompt when stdin isn't a terminal or in `--json` mode
/// (see the CLI-behavior conventions).
pub fn confirm(question: &str, default_yes: bool) -> std::io::Result<bool> {
    use std::io::{BufRead, Write};
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    eprint!("{question} {hint}: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(parse_confirm(&line, default_yes))
}

/// Pure core of [`confirm`], unit-testable without stdin.
fn parse_confirm(answer: &str, default_yes: bool) -> bool {
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        true
    } else if answer.eq_ignore_ascii_case("n") || answer.eq_ignore_ascii_case("no") {
        false
    } else {
        default_yes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_exists_detects_present_and_absent() {
        assert!(tool_exists("sh"));
        assert!(!tool_exists("nonexistent_tool_xyz_123"));
    }

    #[test]
    fn require_tools_path_and_probe_modes() {
        // PATH mode (probe None): present is OK, absent is missing.
        assert!(require_tools(&[("sh", "present", None)]).is_ok());
        assert!(require_tools(&[("nonexistent_zzz", "install zzz", None)]).is_err());

        // Probe mode: `true` runs and exits 0; a missing executable
        // fails the probe. The error lists every missing tool with its
        // hint, and skips the present one.
        assert!(require_tools(&[("true", "ok", Some("--version"))]).is_ok());
        let err = require_tools(&[
            ("true", "ok", Some("--version")),
            ("nonexistent_aaa_111", "install aaa", Some("--version")),
            ("nonexistent_bbb_222", "install bbb", None),
        ])
        .unwrap_err();
        assert!(err.contains("nonexistent_aaa_111"));
        assert!(err.contains("install aaa"));
        assert!(err.contains("nonexistent_bbb_222"));
        assert!(err.contains("install bbb"));
        assert!(!err.contains("true ("));
    }

    #[test]
    fn wrap_prefixed_wraps_and_prefixes() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota";
        let wrapped = wrap_prefixed(text, "> ", 20);
        // Every line is prefixed and within width.
        assert!(wrapped.lines().all(|l| l.starts_with("> ")));
        assert!(wrapped.lines().all(|l| l.chars().count() <= 20));
        // It actually wrapped (more than one line) and lost no words.
        assert!(wrapped.lines().count() > 1);
        assert_eq!(
            wrapped.split_whitespace().count(),
            9 + wrapped.lines().count()
        );
    }

    #[test]
    fn wrap_prefixed_keeps_a_long_word_whole_and_in_place() {
        // A URL longer than the width overflows rather than breaking,
        // and stays with the label that introduces it.
        let url = "https://example.com/a/very/long/path/that/exceeds/the/width";
        let wrapped = wrap_prefixed(&format!("LINK: {url} please"), "  ", 20);
        assert!(wrapped.contains(url), "{wrapped}");
        assert_eq!(wrapped.lines().next().unwrap(), format!("  LINK: {url}"));
        // Wrapping resumes normally after the oversized word.
        assert_eq!(wrapped.lines().nth(1).unwrap(), "  please");
    }

    #[test]
    fn parse_confirm_answers_and_defaults() {
        for yes in ["y", "Y", "yes", "YES", " y "] {
            assert!(parse_confirm(yes, false));
        }
        for no in ["n", "N", "no", "NO"] {
            assert!(!parse_confirm(no, true));
        }
        // Empty (Enter/EOF) and anything unrecognized take the default.
        for other in ["", "\n", "maybe"] {
            assert!(parse_confirm(other, true));
            assert!(!parse_confirm(other, false));
        }
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
