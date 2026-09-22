// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Query Hackage for a package's latest normal (non-deprecated)
//! version — the evidence for the "latest version is packaged" item
//! of a cabal-rpm review. Shells out to `curl` like the crates.io
//! check; parsing is pure so it can be unit-tested offline.

/// `curl` argv for Hackage's preferred-versions JSON: the package's
/// versions split into `normal-version` and `deprecated-version`.
pub fn hackage_argv(name: &str) -> Vec<String> {
    vec![
        "curl".into(),
        "-sf".into(),
        "-A".into(),
        "fedora-review-digest (https://github.com/slopfest/sandogasa)".into(),
        "-H".into(),
        "Accept: application/json".into(),
        format!("https://hackage.haskell.org/package/{name}/preferred"),
    ]
}

/// The highest `normal-version` in a preferred-versions response,
/// compared numerically component by component rather than trusting
/// the order Hackage lists them in. `None` when the body isn't the
/// expected JSON or lists no normal version.
pub fn parse_latest_version(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let key = |s: &str| -> Vec<u64> { s.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    v.get("normal-version")?
        .as_array()?
        .iter()
        .filter_map(|x| x.as_str())
        .max_by_key(|s| key(s))
        .map(str::to_string)
}

/// Fetch and parse the latest version (runs `curl`). `Ok(None)` when
/// the response listed no version; `Err` when curl is missing or the
/// request failed (network down, HTTP error, unknown package).
pub fn fetch_latest_version(name: &str) -> Result<Option<String>, String> {
    let argv = hackage_argv(name);
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| format!("running curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Hackage lookup for {name} failed (curl exit {:?})",
            out.status.code()
        ));
    }
    Ok(parse_latest_version(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_is_the_highest_normal_version_not_the_first_listed() {
        let json =
            r#"{"deprecated-version":["9.9.9"],"normal-version":["0.6.9","0.6.10","0.5.0"]}"#;
        assert_eq!(parse_latest_version(json).as_deref(), Some("0.6.10"));
        assert_eq!(parse_latest_version(r#"{"normal-version":[]}"#), None);
        assert_eq!(parse_latest_version("<html>"), None);
    }

    #[test]
    fn argv_asks_for_json_with_a_user_agent() {
        let argv = hackage_argv("shell-monad");
        assert_eq!(argv[0], "curl");
        assert!(argv.contains(&"Accept: application/json".to_string()));
        assert_eq!(
            argv.last().unwrap(),
            "https://hackage.haskell.org/package/shell-monad/preferred"
        );
    }
}
