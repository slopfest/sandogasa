// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Query PyPI for a project's latest version — the evidence for a
//! Python package's "latest version is packaged" item (#161). Shells out
//! to `curl` as the crates.io and Hackage lookups do; parsing is pure so
//! it can be unit-tested without a network call.

/// `curl` argv to fetch a project's JSON from PyPI. `-f` turns an
/// unknown project or an HTTP error into a non-zero exit.
pub fn pypi_argv(name: &str) -> Vec<String> {
    vec![
        "curl".into(),
        // -L: PyPI redirects a name that is not the canonical one.
        "-sfL".into(),
        "-A".into(),
        "fedora-review-digest (https://github.com/slopfest/sandogasa)".into(),
        format!("https://pypi.org/pypi/{name}/json"),
    ]
}

/// The latest version from a PyPI `/pypi/<name>/json` response:
/// `info.version`, the newest release that is not a pre-release unless
/// there are only pre-releases. `None` if the body isn't the expected
/// JSON.
pub fn parse_latest_version(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("info")?
        .get("version")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Fetch and parse the latest version (runs `curl`). `Ok(None)` when the
/// response carried no version; `Err` when curl is missing or the
/// request failed.
pub fn fetch_latest_version(name: &str) -> Result<Option<String>, String> {
    crate::registry::fetch(&pypi_argv(name), "PyPI", name, parse_latest_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_version_is_info_version() {
        let json = r#"{"info": {"name": "cstq", "version": "0.0.20"}, "releases": {}}"#;
        assert_eq!(parse_latest_version(json).as_deref(), Some("0.0.20"));
        assert_eq!(parse_latest_version(r#"{"info": {"version": ""}}"#), None);
        assert_eq!(parse_latest_version("<html>"), None);
    }

    #[test]
    fn the_request_names_the_project_and_the_tool() {
        let argv = pypi_argv("cstq");
        assert_eq!(argv.last().unwrap(), "https://pypi.org/pypi/cstq/json");
        assert!(argv.iter().any(|a| a.starts_with("fedora-review-digest")));
    }
}
