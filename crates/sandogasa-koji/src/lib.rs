// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Koji build system CLI wrapper.
//!
//! Provides functions for querying Koji tags and builds by shelling
//! out to the `koji` CLI. Supports multiple Koji profiles (e.g.
//! `cbs` for CentOS Build System).

use std::process::Command;

/// A build found in a Koji tag.
#[derive(Debug, Clone)]
pub struct TaggedBuild {
    /// Name-Version-Release.
    pub nvr: String,
    /// Tag the build is in.
    pub tag: String,
    /// FAS username of the builder.
    pub owner: String,
}

/// Parse an NVR string into the package name.
///
/// Returns `None` if the NVR doesn't contain at least two hyphens.
///
/// ```
/// assert_eq!(sandogasa_koji::parse_nvr_name("systemd-256.12-1.fc42"), Some("systemd"));
/// assert_eq!(sandogasa_koji::parse_nvr_name("intel-gpu-tools-1.28-2.el10"), Some("intel-gpu-tools"));
/// assert_eq!(sandogasa_koji::parse_nvr_name("nohyphens"), None);
/// ```
pub fn parse_nvr_name(nvr: &str) -> Option<&str> {
    let mut parts = nvr.rsplitn(3, '-');
    let _release = parts.next()?;
    let _version = parts.next()?;
    let name = parts.next()?;
    if name.is_empty() { None } else { Some(name) }
}

/// Parse an NVR string into (name, version, release).
///
/// ```
/// let (n, v, r) = sandogasa_koji::parse_nvr("systemd-256.12-1.fc42").unwrap();
/// assert_eq!(n, "systemd");
/// assert_eq!(v, "256.12");
/// assert_eq!(r, "1.fc42");
/// ```
pub fn parse_nvr(nvr: &str) -> Option<(&str, &str, &str)> {
    let mut parts = nvr.rsplitn(3, '-');
    let release = parts.next()?;
    let version = parts.next()?;
    let name = parts.next()?;
    if name.is_empty() {
        None
    } else {
        Some((name, version, release))
    }
}

/// Run a koji command with optional profile and return stdout.
fn run_koji(profile: Option<&str>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("koji");
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    cmd.args(args);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run koji: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "koji {} failed: {}",
            args.first().unwrap_or(&""),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// List builds in a Koji tag with their owners.
///
/// Returns the NVR, tag, and owner for each build.
/// Uses `--latest` to only show the latest build of each package.
/// If `timestamp` is given, queries the tag state at that Unix
/// timestamp.
pub fn list_tagged(
    tag: &str,
    profile: Option<&str>,
    timestamp: Option<i64>,
) -> Result<Vec<TaggedBuild>, String> {
    let ts_str = timestamp.map(|t| t.to_string());
    let mut args = vec!["list-tagged", "--latest"];
    if let Some(ref ts) = ts_str {
        args.push("--ts");
        args.push(ts);
    }
    args.push(tag);
    let stdout = run_koji(profile, &args)?;
    let mut builds = Vec::new();

    for line in stdout.lines().skip(2) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            builds.push(TaggedBuild {
                nvr: parts[0].to_string(),
                tag: parts[1].to_string(),
                owner: parts[2].to_string(),
            });
        }
    }

    Ok(builds)
}

/// Untag a build from a Koji tag (`koji untag-build <tag> <nvr>`).
///
/// Succeeds silently when Koji accepts the command; returns
/// the koji stderr otherwise. No-op on whether the build was
/// actually present beforehand — Koji tolerates re-untagging.
pub fn untag_build(tag: &str, nvr: &str, profile: Option<&str>) -> Result<(), String> {
    run_koji(profile, &["untag-build", tag, nvr])?;
    Ok(())
}

/// List NVRs in a Koji tag (quiet mode, NVRs only).
pub fn list_tagged_nvrs(tag: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let stdout = run_koji(profile, &["list-tagged", "--quiet", tag])?;
    Ok(stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect())
}

/// Parse the build's creation date from `koji buildinfo`.
///
/// Looks for a `Creation time: YYYY-MM-DD HH:MM:SS` line and
/// returns the date portion. Returns `Ok(None)` if the line
/// isn't present or can't be parsed — callers should treat
/// that as "unknown date" rather than an error.
pub fn build_creation_date(
    nvr: &str,
    profile: Option<&str>,
) -> Result<Option<chrono::NaiveDate>, String> {
    let stdout = run_koji(profile, &["buildinfo", nvr])?;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("Creation time:") {
            let value = rest.trim();
            // Parse `YYYY-MM-DD HH:MM:SS` — take the date part.
            let date_part = value.split_whitespace().next().unwrap_or("");
            if let Ok(date) = chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d") {
                return Ok(Some(date));
            }
        }
    }
    Ok(None)
}

/// List binary RPM names for a build via `koji buildinfo`.
///
/// Parses the RPMs section, returning binary package names
/// (excluding `.src.rpm` entries).
pub fn build_rpms(nvr: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let stdout = run_koji(profile, &["buildinfo", nvr])?;

    let mut in_rpms = false;
    let mut names = Vec::new();
    for line in stdout.lines() {
        if line.starts_with("RPMs:") {
            in_rpms = true;
            continue;
        }
        if !in_rpms {
            continue;
        }
        let path = line.split('\t').next().unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        let filename = path.rsplit('/').next().unwrap_or(path);
        if filename.ends_with(".src.rpm") {
            continue;
        }
        if let Some(without_rpm) = filename.strip_suffix(".rpm")
            && let Some(dot_pos) = without_rpm.rfind('.')
            && let Some(name) = parse_nvr_name(&without_rpm[..dot_pos])
        {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nvr_name_standard() {
        assert_eq!(parse_nvr_name("systemd-256.12-1.fc42"), Some("systemd"));
    }

    #[test]
    fn parse_nvr_name_hyphenated() {
        assert_eq!(
            parse_nvr_name("intel-gpu-tools-1.28-2.el10"),
            Some("intel-gpu-tools")
        );
    }

    #[test]
    fn parse_nvr_name_too_short() {
        assert_eq!(parse_nvr_name("nohyphens"), None);
        assert_eq!(parse_nvr_name("one-hyphen"), None);
    }

    #[test]
    fn parse_nvr_full() {
        let (n, v, r) = parse_nvr("systemd-256.12-1.fc42").unwrap();
        assert_eq!(n, "systemd");
        assert_eq!(v, "256.12");
        assert_eq!(r, "1.fc42");
    }

    #[test]
    fn parse_nvr_full_hyphenated() {
        let (n, v, r) = parse_nvr("intel-gpu-tools-1.28-2.hs.el10").unwrap();
        assert_eq!(n, "intel-gpu-tools");
        assert_eq!(v, "1.28");
        assert_eq!(r, "2.hs.el10");
    }

    #[test]
    fn parse_nvr_full_too_short() {
        assert!(parse_nvr("nohyphens").is_none());
    }
}
