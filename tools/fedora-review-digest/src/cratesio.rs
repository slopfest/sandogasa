// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Query crates.io for a crate's latest stable version — the evidence
//! for the "latest version is packaged" checklist item. Shells out to
//! `curl` (transparent, no HTTP dependency) with a descriptive
//! User-Agent, as crates.io's data-access policy requires. Parsing is
//! pure so it can be unit-tested without a network call.

/// `curl` argv to fetch a crate's metadata JSON from the crates.io API.
/// `-f` turns an unknown crate / HTTP error into a non-zero exit; the
/// User-Agent identifies the tool per <https://crates.io/data-access>.
pub fn crates_io_argv(name: &str) -> Vec<String> {
    vec![
        "curl".into(),
        "-sf".into(),
        "-A".into(),
        "fedora-review-digest (https://github.com/slopfest/sandogasa)".into(),
        format!("https://crates.io/api/v1/crates/{name}"),
    ]
}

/// The latest stable version from a crates.io `/crates/<name>` response:
/// `crate.max_stable_version`, falling back to `max_version` then
/// `newest_version`. `None` if the body isn't the expected JSON or has
/// no usable version.
pub fn parse_max_stable_version(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let c = v.get("crate")?;
    ["max_stable_version", "max_version", "newest_version"]
        .iter()
        .find_map(|k| {
            c.get(k)
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
}

/// Fetch and parse the latest stable version (runs `curl`). `Ok(None)`
/// when the response carried no version; `Err` when curl is missing or
/// the request failed (network down, HTTP error, unknown crate).
pub fn fetch_max_stable_version(name: &str) -> Result<Option<String>, String> {
    let argv = crates_io_argv(name);
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| format!("running curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "crates.io lookup for {name} failed (curl exit {:?})",
            out.status.code()
        ));
    }
    Ok(parse_max_stable_version(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_has_useragent_and_url() {
        let a = crates_io_argv("trustfall_core");
        assert_eq!(a[0], "curl");
        assert!(a.iter().any(|x| x == "-A"));
        assert!(a.last().unwrap().ends_with("/api/v1/crates/trustfall_core"));
    }

    #[test]
    fn parse_prefers_max_stable_then_falls_back() {
        assert_eq!(
            parse_max_stable_version(
                r#"{"crate": {"max_stable_version": "0.8.1", "max_version": "0.9.0-rc1"}}"#
            )
            .as_deref(),
            Some("0.8.1")
        );
        // max_stable null (only pre-releases) → fall back to max_version.
        assert_eq!(
            parse_max_stable_version(
                r#"{"crate": {"max_stable_version": null, "max_version": "0.9.0-rc1"}}"#
            )
            .as_deref(),
            Some("0.9.0-rc1")
        );
        assert_eq!(parse_max_stable_version("not json"), None);
        assert_eq!(parse_max_stable_version(r#"{"errors": []}"#), None);
    }
}
